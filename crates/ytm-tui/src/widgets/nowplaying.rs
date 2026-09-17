//! The bottom bar: what is playing, where we are in it.

use crate::{
    app::AppState,
    theme::Theme,
    util::text::{display_width, truncate_to_width},
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use ytm_player::player::{PlaybackState, RepeatMode};

/// Rows inside the bar, in order: blank margin, rule, title, gap, progress. `render`
/// reserves `HEIGHT` and turns a click into a position from `PROGRESS_ROW`, so
/// these are the single source of truth — a copy drifts and a click seeks wrong.
pub const RULE_ROW: u16 = 1;
pub const TITLE_ROW: u16 = 2;
pub const PROGRESS_ROW: u16 = 4;
pub const HEIGHT: u16 = 5;

/// Eighth-block glyphs give 8x the resolution of a plain block per column.
const EIGHTHS: [char; 8] = [
    '\u{258F}', '\u{258E}', '\u{258D}', '\u{258C}', '\u{258B}', '\u{258A}', '\u{2589}', '\u{2588}',
];

/// The fixed text around the bar: elapsed, duration, and the mode flags. Split
/// out so the click handler measures these widths rather than re-deriving them;
/// a copy there drifts when the flags change and a seek misses the click.
fn chrome_parts(s: &AppState) -> (String, String, String) {
    (
        format!("{}  ", s.position),
        format!("  {}", s.duration),
        format!(
            "  {}{}  {:>3}%",
            if s.shuffle { "\u{21C4}" } else { " " },
            match s.repeat {
                RepeatMode::Off => " ",
                RepeatMode::One => "\u{2460}",
                RepeatMode::All => "\u{21BB}",
            },
            if s.muted { 0 } else { s.volume },
        ),
    )
}

/// Column the bar starts at, and how wide it is, inside a row of width `w`.
pub fn bar_span(w: usize, s: &AppState) -> (usize, usize) {
    let (times, tail, flags) = chrome_parts(s);
    // Columns, not bytes: the flag glyphs are multi-byte but one column wide.
    let chrome = display_width(&times) + display_width(&tail) + display_width(&flags);
    (display_width(&times), w.saturating_sub(chrome))
}

fn bar_width(w: usize, s: &AppState) -> usize {
    bar_span(w, s).1
}

/// Which second a click at column `col` of the progress row means. `None` when
/// the click misses the bar, or when nothing is playing and there is no duration
/// to seek within — a fraction of zero is not a position.
pub fn seek_target_secs(w: usize, col: usize, s: &AppState) -> Option<u64> {
    let dur = s.duration.as_secs();
    if dur == 0 {
        return None;
    }
    let (start, width) = bar_span(w, s);
    if width == 0 || col < start || col >= start + width {
        return None;
    }
    // The click lands *on* a cell, so its left edge is the position it means:
    // clicking the first cell seeks to 0, not to half a cell in.
    let ratio = (col - start) as f64 / width as f64;
    Some((ratio * dur as f64).round() as u64)
}

pub fn progress_bar(ratio: f64, width: usize) -> String {
    let r = if ratio.is_nan() {
        0.0
    } else {
        ratio.clamp(0.0, 1.0)
    };
    let total_eighths = (r * (width * 8) as f64).round() as usize;
    let full = total_eighths / 8;
    let rem = total_eighths % 8;

    let mut s: String = std::iter::repeat_n('\u{2588}', full.min(width)).collect();
    let partial = full < width && rem > 0;
    if partial {
        s.push(EIGHTHS[rem - 1]);
    }
    // Pad with spaces so the bar always occupies its full width.
    let filled = full.min(width) + usize::from(partial);
    s.extend(std::iter::repeat_n(' ', width.saturating_sub(filled)));
    s
}

pub fn draw(f: &mut Frame, area: Rect, s: &AppState, t: &Theme) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    // Margin, rule, title, blank gap, progress. The blank row above the rule is
    // what keeps the rule from sitting flush against the last track; the gap row
    // between title and progress gives breathing room so the seekbar and title
    // do not crowd together.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(area);

    let w = area.width as usize;

    // Full width: a rule stopping at the sidebar reads as part of the list. The
    // sidebar divider continues through the margin row and meets it in a
    // `\u{2534}` junction — abutting lines read as two lines, not one frame.
    let div = crate::render::SIDEBAR_WIDTH as usize;
    if area.height > RULE_ROW {
        let mut rule: String = "\u{2500}".repeat(w);
        if w > div {
            rule = rule
                .chars()
                .enumerate()
                .map(|(i, c)| if i == div { '\u{2534}' } else { c })
                .collect();
        }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                rule,
                Style::default().fg(t.fg_dim),
            ))),
            rows[RULE_ROW as usize],
        );
        // Only the divider crosses the margin; the list side stays empty, which
        // is the breathing room the gap is for.
        if w > div {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::raw(" ".repeat(div)),
                    Span::styled("\u{2502}", Style::default().fg(t.fg_dim)),
                ])),
                rows[0],
            );
        }
    }

    // Title -- artist, plus the state glyph.
    let line = match &s.now_playing {
        None => Line::from(Span::styled(
            "Nothing playing",
            Style::default().fg(t.fg_dim),
        )),
        Some(track) => {
            let glyph = match s.playback {
                PlaybackState::Playing => "\u{25B6}",
                PlaybackState::Paused => "\u{23F8}",
                PlaybackState::Loading => "\u{22EF}",
                PlaybackState::Stopped => "\u{25A0}",
            };
            let title = truncate_to_width(&track.title, w.saturating_sub(24));
            Line::from(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(t.accent)),
                Span::styled(
                    title,
                    Style::default()
                        .fg(t.fg_bright)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" \u{2014} ", Style::default().fg(t.fg_dim)),
                Span::styled(track.artist_display(), Style::default().fg(t.fg)),
            ])
        }
    };
    f.render_widget(Paragraph::new(line), rows[TITLE_ROW as usize]);

    // Row 2: elapsed, bar, duration, then the mode flags.
    let pos = s.position.as_secs();
    let dur = s.duration.as_secs();
    let ratio = if dur == 0 {
        0.0
    } else {
        pos as f64 / dur as f64
    };

    let (times, tail, _) = chrome_parts(s);
    let bar_w = bar_width(w, s);
    let mut spans = vec![
        Span::styled(times, Style::default().fg(t.fg_dim)),
        Span::styled(progress_bar(ratio, bar_w), Style::default().fg(t.accent)),
        Span::styled(tail, Style::default().fg(t.fg_dim)),
    ];
    spans.extend(flag_spans(s, t));
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        rows[PROGRESS_ROW as usize],
    );
}

/// The mode flags as separately styled spans: shuffle, repeat, and volume. The
/// concatenated text must match `chrome_parts`'s third field to the column — the
/// seek geometry measures that — but each flag takes the accent when it is on.
fn flag_spans(s: &AppState, t: &Theme) -> Vec<Span<'static>> {
    let dim = Style::default().fg(t.fg_dim);
    let on = Style::default().fg(t.accent).add_modifier(Modifier::BOLD);
    let (repeat_glyph, repeat_on) = match s.repeat {
        RepeatMode::Off => (" ", false),
        RepeatMode::One => ("\u{2460}", true),
        RepeatMode::All => ("\u{21BB}", true),
    };
    vec![
        Span::styled("  ", dim),
        Span::styled(
            if s.shuffle { "\u{21C4}" } else { " " },
            if s.shuffle { on } else { dim },
        ),
        Span::styled(repeat_glyph, if repeat_on { on } else { dim }),
        Span::styled("  ", dim),
        // Muted reads as an alert, not a mode: the error colour and a 0 say the
        // sound is off rather than merely quiet.
        Span::styled(
            format!("{:>3}%", if s.muted { 0 } else { s.volume }),
            if s.muted {
                Style::default().fg(t.error)
            } else {
                dim
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::AppState, theme::Theme};
    use ratatui::{Terminal, backend::TestBackend};
    use ytm_core::{Track, TrackDuration};
    use ytm_player::player::PlaybackState;

    fn buffer_text(state: &AppState) -> String {
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                state,
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

    /// The rendered frame as one string per screen row.
    fn buffer_rows(state: &AppState, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                state,
                &theme,
                &crate::keymap::KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
        let cells: Vec<String> = t
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol().to_owned())
            .collect();
        cells.chunks(w as usize).map(|r| r.concat()).collect()
    }

    /// Index of the separator row, by the only row made mostly of rule glyphs.
    fn rule_row(rows: &[String]) -> Option<usize> {
        rows.iter()
            .position(|r| r.chars().filter(|c| *c == '\u{2500}').count() > 40)
    }

    #[test]
    fn a_rule_separates_the_now_playing_bar_from_the_list() {
        let rows = buffer_rows(&playing_two_minutes(), 80, 24);
        assert!(
            rule_row(&rows).is_some(),
            "a horizontal separator must sit above the bar, got:\n{}",
            rows.join("\n")
        );
    }

    #[test]
    fn the_rule_has_a_blank_row_of_margin_above_it() {
        // Owner's note on the mockup: the line they drew had no breathing room.
        // One row, not two — the list is what pays for the space.
        let rows = buffer_rows(&playing_two_minutes(), 80, 24);
        let i = rule_row(&rows).expect("a rule row");
        assert!(
            rows[i - 1].replace('\u{2502}', "").trim().is_empty(),
            "the row above the rule is margin apart from the divider, got {:?}",
            rows[i - 1]
        );
        assert!(
            rows[i + 1].contains("Roygbiv"),
            "the title follows the rule, got {:?}",
            rows[i + 1]
        );
        assert!(
            rows[i + 2].trim().is_empty(),
            "the row between title and seekbar is a blank gap, got {:?}",
            rows[i + 2]
        );
        assert!(
            rows[i + 3].contains("0:00") && rows[i + 3].contains("2:00"),
            "the progress bar follows the gap, got {:?}",
            rows[i + 3]
        );
    }

    #[test]
    fn the_rule_spans_the_full_width() {
        // A rule that stopped at the sidebar would read as part of the list.
        // Every column is rule or the junction where the sidebar divider lands.
        let rows = buffer_rows(&playing_two_minutes(), 80, 24);
        let i = rule_row(&rows).expect("a rule row");
        let line: Vec<char> = rows[i].chars().collect();
        assert_eq!(line.len(), 80);
        assert!(
            line.iter().all(|c| *c == '\u{2500}' || *c == '\u{2534}'),
            "the whole row must be rule, got {:?}",
            rows[i]
        );
    }

    #[test]
    fn the_sidebar_divider_reaches_down_to_the_rule() {
        // The margin row must not break the vertical line: it stopped a row short
        // of the rule, leaving a visible gap the owner asked to close.
        let rows = buffer_rows(&playing_two_minutes(), 80, 24);
        let i = rule_row(&rows).expect("a rule row");
        let col = crate::render::SIDEBAR_WIDTH as usize;
        let above: Vec<char> = rows[i - 1].chars().collect();
        assert_eq!(
            above[col],
            '\u{2502}',
            "the divider must continue through the margin row, got {:?}",
            rows[i - 1]
        );
    }

    #[test]
    fn the_two_rules_meet_in_a_junction() {
        // Abutting lines read as two lines; a junction reads as one frame.
        let rows = buffer_rows(&playing_two_minutes(), 80, 24);
        let i = rule_row(&rows).expect("a rule row");
        let col = crate::render::SIDEBAR_WIDTH as usize;
        let line: Vec<char> = rows[i].chars().collect();
        assert_eq!(
            line[col], '\u{2534}',
            "expected the up-tee, got {:?}",
            rows[i]
        );
    }

    #[test]
    fn the_margin_row_is_still_blank_beside_the_divider() {
        // The gap the owner wanted is between the last track and the rule; only
        // the divider crosses it.
        let rows = buffer_rows(&playing_two_minutes(), 80, 24);
        let i = rule_row(&rows).expect("a rule row");
        let col = crate::render::SIDEBAR_WIDTH as usize;
        let above: Vec<char> = rows[i - 1].chars().collect();
        assert!(
            above[col + 1..].iter().all(|c| c.is_whitespace()),
            "the list side of the margin row must stay empty, got {:?}",
            rows[i - 1]
        );
    }

    #[test]
    fn progress_bar_is_empty_at_zero_and_full_at_one() {
        assert_eq!(progress_bar(0.0, 10).trim_end(), "");
        assert_eq!(progress_bar(1.0, 10), "██████████");
    }

    #[test]
    fn progress_bar_uses_partial_blocks_for_sub_cell_precision() {
        // Spec §6: eighth-blocks, not '='. The plan's `'\u{258F}'..='\u{2588}'`
        // range is inverted and therefore empty, so it could never fail; the seven
        // partial glyphs are what it meant, and a bar of full blocks fails them.
        let b = progress_bar(0.55, 10);
        assert!(b.chars().any(|c| EIGHTHS[..7].contains(&c)), "got {b:?}");
    }

    #[test]
    fn progress_bar_clamps_out_of_range_ratios() {
        assert_eq!(progress_bar(-1.0, 5).trim_end(), "");
        assert_eq!(progress_bar(2.0, 5), "█████");
    }

    fn playing_two_minutes() -> AppState {
        AppState {
            now_playing: Some(Track::stub("v1", "Roygbiv")),
            playback: PlaybackState::Playing,
            position: TrackDuration::from_secs(0),
            duration: TrackDuration::from_secs(120),
            ..Default::default()
        }
    }

    #[test]
    fn a_click_at_the_bars_left_edge_seeks_to_the_start() {
        // The click lands *on* a cell, so the cell's left edge is the position it
        // means. Measuring from its centre would make the first cell seek a few
        // seconds in, and clicking "the very beginning" would not rewind fully.
        let s = playing_two_minutes();
        let (start, _) = bar_span(80, &s);
        assert_eq!(seek_target_secs(80, start, &s), Some(0));
    }

    #[test]
    fn a_click_halfway_along_the_bar_seeks_to_the_middle() {
        let s = playing_two_minutes();
        let (start, width) = bar_span(80, &s);
        let got = seek_target_secs(80, start + width / 2, &s).expect("inside the bar");
        // Within a second of the midpoint; the exact value depends on bar width.
        assert!(got.abs_diff(60) <= 1, "expected ~60s, got {got}");
    }

    #[test]
    fn a_click_on_the_chrome_either_side_of_the_bar_does_not_seek() {
        // The elapsed and duration columns are not the bar. Treating them as its
        // ends would make a click on "2:00" jump to a place the user did not aim
        // at.
        let s = playing_two_minutes();
        let (start, width) = bar_span(80, &s);
        assert_eq!(seek_target_secs(80, start.saturating_sub(1), &s), None);
        assert_eq!(seek_target_secs(80, start + width, &s), None);
    }

    #[test]
    fn a_click_does_not_seek_when_nothing_is_playing() {
        // Duration is zero, so there is no position a fraction could mean.
        let s = AppState::default();
        assert_eq!(seek_target_secs(80, 20, &s), None);
    }

    #[test]
    fn the_bar_span_matches_the_bar_that_is_drawn() {
        // If these disagree, a click seeks to a different point than the one
        // under the pointer — the whole reason the geometry has one owner.
        let s = playing_two_minutes();
        let (_, width) = bar_span(80, &s);
        assert_eq!(progress_bar(0.5, width).chars().count(), width);
    }

    #[test]
    fn now_playing_shows_title_artist_and_times() {
        let s = AppState {
            now_playing: Some(Track::stub("v1", "Roygbiv")),
            playback: PlaybackState::Playing,
            position: TrackDuration::from_secs(65),
            duration: TrackDuration::from_secs(149),
            ..Default::default()
        };
        let text = buffer_text(&s);
        assert!(text.contains("Roygbiv"), "title missing");
        assert!(text.contains("1:05"), "elapsed missing");
        assert!(text.contains("2:29"), "duration missing");
    }

    #[test]
    fn idle_state_shows_a_placeholder_not_an_empty_bar() {
        let s = AppState::default();
        assert!(buffer_text(&s).contains("Nothing playing"));
    }

    /// The colour of the first cell showing `sym`, searched from the right so
    /// the flags at the end of the row are found rather than a stray match in a
    /// title.
    fn last_cell_color(state: &AppState, sym: &str) -> Option<ratatui::style::Color> {
        let mut t = Terminal::new(TestBackend::new(80, 20)).unwrap();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                state,
                &theme,
                &crate::keymap::KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
        let buf = t.backend().buffer().clone();
        buf.content()
            .iter()
            .rev()
            .find(|c| c.symbol() == sym)
            .map(|c| c.fg)
    }

    #[test]
    fn the_flags_geometry_matches_chrome_parts_so_seeking_stays_exact() {
        // The styled flag spans must concatenate to exactly the third field of
        // chrome_parts — the seek hit-test measures that string, so any drift
        // would make a click land on the wrong second.
        let s = AppState {
            shuffle: true,
            repeat: RepeatMode::All,
            volume: 80,
            ..playing_two_minutes()
        };
        let theme = Theme::default();
        let joined: String = super::flag_spans(&s, &theme)
            .iter()
            .map(|sp| sp.content.as_ref())
            .collect();
        let (_, _, flags) = super::chrome_parts(&s);
        assert_eq!(joined, flags, "styled flags must not change the widths");
    }

    #[test]
    fn an_active_mode_is_drawn_in_the_accent_not_a_uniform_dim() {
        // FR-U: the user must be able to tell shuffle is on at a glance, not by
        // reading a glyph. On uses the accent; off stays dim.
        let theme = Theme::default();
        let on = last_cell_color(
            &AppState {
                shuffle: true,
                ..playing_two_minutes()
            },
            "\u{21C4}",
        );
        assert_eq!(on, Some(theme.accent), "shuffle-on should take the accent");
    }

    #[test]
    fn muting_shows_zero_in_the_error_colour() {
        // Muted is an alert, not just a low volume: the error colour and a 0
        // together say the sound is off.
        let theme = Theme::default();
        let s = AppState {
            muted: true,
            volume: 60,
            ..playing_two_minutes()
        };
        let text = buffer_text(&s);
        assert!(text.contains("0%"), "muted reads as 0%, got: {text}");
        assert_eq!(
            last_cell_color(&s, "0"),
            Some(theme.error),
            "the muted volume should be drawn in the error colour"
        );
    }

    #[test]
    fn sidebar_lists_every_source() {
        let text = buffer_text(&AppState::default());
        // "Fav" rather than "Songs" at the owner's request, and Home leads.
        for label in [
            "Home",
            "Playlists",
            "Fav",
            "Albums",
            "Artists",
            "Search",
            "Queue",
        ] {
            assert!(text.contains(label), "sidebar missing {label}");
        }
    }

    #[test]
    fn a_very_narrow_terminal_does_not_panic() {
        // Users resize to absurd sizes; a panic here loses their session.
        let mut t = Terminal::new(TestBackend::new(8, 4)).unwrap();
        let s = AppState::default();
        let theme = Theme::default();
        t.draw(|f| {
            crate::render::render(
                f,
                &s,
                &theme,
                &crate::keymap::KeyMap::default(),
                &mut crate::widgets::art::ArtCache::disabled(),
            )
        })
        .unwrap();
    }
}
