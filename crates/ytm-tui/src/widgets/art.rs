//! Album art via ratatui-image. Optional by nature: many terminals cannot
//! display images, and the UI must be complete without it (FR-U5).
//!
//! Nothing here is allowed to be load-bearing. A terminal with no image support,
//! a track with no thumbnail, a dead URL, and a pane too narrow to be legible
//! all take the same path: draw nothing, say nothing, and leave the layout
//! exactly as it would have been.

use ratatui::{Frame, layout::Rect};
use ratatui_image::{StatefulImage, picker::Picker, protocol::StatefulProtocol};
use std::collections::{HashMap, HashSet, VecDeque};

/// True when running inside tmux, where probing stdio breaks key input.
fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some() || std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux"))
}

/// Below this the image is noise, not art.
const MIN_W: u16 = 10;
const MIN_H: u16 = 5;

pub fn should_draw(area: Rect) -> bool {
    area.width >= MIN_W && area.height >= MIN_H
}

pub struct ArtCache {
    /// `None` when the terminal cannot display images. Also the picker that
    /// builds protocol objects, so "enabled" and "can decode" cannot disagree.
    picker: Option<Picker>,
    /// Decoded protocol objects, keyed by URL. Bounded — see `MAX_IMAGES`.
    images: HashMap<String, StatefulProtocol>,
    /// Insertion order, so the cap can evict. Only the now-playing URL is ever
    /// drawn and it is always the newest, so recency and insertion order agree.
    order: VecDeque<String>,
    in_flight: HashSet<String>,
    failed: HashSet<String>,

    /// Covers for the two-line list rows. Kept apart from `images` so a
    /// screenful of covers can never evict the now-playing image. Downscaled to
    /// 96px, so the cap is kilobytes.
    rows: HashMap<String, StatefulProtocol>,
    row_order: VecDeque<String>,
    row_in_flight: HashSet<String>,
    row_failed: HashSet<String>,
}

impl ArtCache {
    /// Each entry retains a full decoded image at `ART_PX` (600x600x3 ≈ 1 MiB),
    /// so an unbounded map cost ~1 MiB per track played. 16 is ~16 MiB and far
    /// more history than the one visible panel can use.
    pub const MAX_IMAGES: usize = 16;

    /// Several screenfuls of two-line rows. Covers are downscaled to 96px, so
    /// 128 entries stay in the low hundreds of kilobytes and scrolling back up
    /// a list does not re-download every cover.
    pub const MAX_ROWS: usize = 128;

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn row_len(&self) -> usize {
        self.rows.len()
    }

    /// Enabled without probing the terminal. Halfblocks need no protocol
    /// support, which is what makes an enabled cache testable headless.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::with_picker(Some(Picker::halfblocks()))
    }
}

impl ArtCache {
    /// Probes the terminal, returning a disabled cache when unsupported. Call
    /// after entering the alternate screen but before reading events: the query
    /// consumes stdin, can block up to 2s, and must follow the first frame (NFR-1).
    pub fn detect() -> Self {
        // Probing stdio under tmux left crossterm delivering no key presses, so
        // Enter did nothing and nothing played at all. Measured live: art on = no
        // playback, art off = plays. Halfblocks need no probe, so use them here.
        if in_tmux() {
            tracing::info!("tmux detected, using halfblocks without probing stdio");
            return Self::with_picker(Some(Picker::halfblocks()));
        }
        match Picker::from_query_stdio() {
            Ok(picker) => {
                tracing::info!(protocol = ?picker.protocol_type(), "album art enabled");
                Self::with_picker(Some(picker))
            }
            Err(e) => {
                // Not an error the user needs to see: FR-U5 makes art optional.
                tracing::info!(reason = %e, "album art unavailable, continuing without it");
                Self::with_picker(None)
            }
        }
    }

    pub fn disabled() -> Self {
        Self::with_picker(None)
    }

    fn with_picker(picker: Option<Picker>) -> Self {
        Self {
            picker,
            images: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashSet::new(),
            failed: HashSet::new(),
            rows: HashMap::new(),
            row_order: VecDeque::new(),
            row_in_flight: HashSet::new(),
            row_failed: HashSet::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.picker.is_some()
    }

    pub fn get(&self, url: &str) -> Option<&StatefulProtocol> {
        self.images.get(url)
    }

    pub fn get_row(&self, url: &str) -> Option<&StatefulProtocol> {
        self.rows.get(url)
    }

    /// True exactly once per URL, unless it later fails.
    pub fn should_fetch(&mut self, url: &str) -> bool {
        if self.images.contains_key(url)
            || self.in_flight.contains(url)
            || self.failed.contains(url)
        {
            return false;
        }
        self.in_flight.insert(url.to_owned());
        true
    }

    pub fn mark_failed(&mut self, url: &str) {
        self.in_flight.remove(url);
        self.failed.insert(url.to_owned());
    }

    /// Mark a row cover as just drawn, so eviction drops the least recently
    /// *seen* cover. Insertion-order eviction would drop the top of the list
    /// while the user scrolls back up through it.
    fn touch_row(&mut self, url: &str) {
        if !self.rows.contains_key(url) {
            return;
        }
        if let Some(pos) = self.row_order.iter().position(|u| u == url)
            && let Some(entry) = self.row_order.remove(pos)
        {
            self.row_order.push_back(entry);
        }
    }

    /// True exactly once per row-cover URL, unless it later fails.
    pub fn should_fetch_row(&mut self, url: &str) -> bool {
        if self.rows.contains_key(url)
            || self.row_in_flight.contains(url)
            || self.row_failed.contains(url)
        {
            return false;
        }
        self.row_in_flight.insert(url.to_owned());
        true
    }

    pub fn mark_row_failed(&mut self, url: &str) {
        self.row_in_flight.remove(url);
        self.row_failed.insert(url.to_owned());
    }

    /// Build the protocol object for a decoded image. A disabled cache drops it —
    /// nothing could render it, and it would leak ~1 MiB per track played. An
    /// enabled one evicts the oldest past the cap, for the same reason.
    pub fn insert(&mut self, url: &str, image: image::DynamicImage) {
        self.in_flight.remove(url);
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let proto = picker.new_resize_protocol(image);
        if self.images.insert(url.to_owned(), proto).is_none() {
            // Only track order for a genuinely new key, or a re-request would
            // queue the same URL twice and evict a live image early.
            self.order.push_back(url.to_owned());
        }
        while self.order.len() > Self::MAX_IMAGES {
            if let Some(oldest) = self.order.pop_front() {
                self.images.remove(&oldest);
            }
        }
    }

    /// Build the protocol object for a two-line row's cover. Downscaled to at
    /// most 96px so a screenful is kilobytes. A disabled cache stores nothing.
    /// Entries here never evict now-playing art: the maps are separate, which
    /// is the whole point of the second cache.
    pub fn insert_row(&mut self, url: &str, image: image::DynamicImage) {
        self.row_in_flight.remove(url);
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let downscaled = downscale_for_rows(image);
        let proto = picker.new_resize_protocol(downscaled);
        if self.rows.insert(url.to_owned(), proto).is_none() {
            self.row_order.push_back(url.to_owned());
        }
        while self.row_order.len() > Self::MAX_ROWS {
            if let Some(oldest) = self.row_order.pop_front() {
                self.rows.remove(&oldest);
            }
        }
    }
}

/// Draw the art for whatever is playing, or nothing at all. Takes `&mut ArtCache`
/// because the protocol objects re-encode themselves when the area changes — that
/// is what makes the image survive a resize.
pub fn draw(f: &mut Frame, area: Rect, url: Option<&str>, art: &mut ArtCache) {
    if !should_draw(area) || !art.is_enabled() {
        return;
    }
    let Some(url) = url else { return };
    let Some(proto) = art.images.get_mut(url) else {
        return;
    };
    f.render_stateful_widget(StatefulImage::default(), area, proto);
}

/// A list-row cover is four columns wide, never 600px. Fitting it inside 96px
/// keeps a screenful of covers down to kilobytes without visible blur.
fn downscale_for_rows(image: image::DynamicImage) -> image::DynamicImage {
    image.thumbnail(
        crate::widgets::row::ROW_ART_PX,
        crate::widgets::row::ROW_ART_PX,
    )
}

/// Draw one list row's cover, or nothing at all. Deliberately bypasses
/// [`should_draw`]: a 4x2 rect is below the panel minimum but is exactly the
/// size a row cover is meant to be.
pub fn draw_row(f: &mut Frame, area: Rect, url: Option<&str>, art: &mut ArtCache) {
    if !art.is_enabled() || area.width == 0 || area.height == 0 {
        return;
    }
    let Some(url) = url else { return };
    art.touch_row(url);
    let Some(proto) = art.rows.get_mut(url) else {
        return;
    };
    f.render_stateful_widget(StatefulImage::default(), area, proto);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny real image; `insert` needs a DynamicImage and the halfblocks
    /// picker needs no terminal support, so this works headless.
    fn tiny(n: u8) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 2, image::Rgb([n, n, n])))
    }

    #[test]
    fn the_cache_is_bounded_so_a_long_session_cannot_exhaust_memory() {
        // Each entry retains a full decoded 600x600 image (~1 MiB). Unbounded,
        // an evening of 100 tracks held ~100 MiB and 500 held ~500 MiB.
        let mut c = ArtCache::for_test();
        for i in 0..(ArtCache::MAX_IMAGES + 20) {
            c.insert(&format!("u{i}"), tiny(i as u8));
        }
        assert_eq!(
            c.len(),
            ArtCache::MAX_IMAGES,
            "the cache must stop growing at the cap"
        );
    }

    #[test]
    fn eviction_drops_the_oldest_and_keeps_the_newest() {
        // The now-playing URL is always the most recent insert, so the entry
        // that is actually drawn must never be the one evicted.
        let mut c = ArtCache::for_test();
        for i in 0..(ArtCache::MAX_IMAGES + 1) {
            c.insert(&format!("u{i}"), tiny(i as u8));
        }
        assert!(c.get("u0").is_none(), "the oldest must have been evicted");
        assert!(
            c.get(&format!("u{}", ArtCache::MAX_IMAGES)).is_some(),
            "the newest must still be there"
        );
    }

    #[test]
    fn reinserting_a_url_does_not_grow_the_order_queue() {
        // art_tick can re-request a URL after a failure, and a duplicate entry
        // in the order queue would evict a live image early.
        let mut c = ArtCache::for_test();
        for _ in 0..(ArtCache::MAX_IMAGES + 5) {
            c.insert("same", tiny(1));
        }
        assert_eq!(c.len(), 1, "one URL is one entry however often it arrives");
    }

    #[test]
    fn a_disabled_cache_still_stores_nothing() {
        // The existing guarantee: with no picker there is nothing that could
        // render an image, so keeping it would leak for every track played.
        let mut c = ArtCache::disabled();
        c.insert("u1", tiny(1));
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn art_is_skipped_when_the_terminal_cannot_display_images() {
        // FR-U5: absence must be graceful, never an error or a broken box.
        let cache = ArtCache::disabled();
        assert!(!cache.is_enabled());
        assert!(cache.get("https://example.com/a.jpg").is_none());
    }

    #[test]
    fn a_url_is_requested_only_once() {
        let mut cache = ArtCache::disabled();
        assert!(cache.should_fetch("u1"), "first sighting fetches");
        assert!(!cache.should_fetch("u1"), "already in flight or cached");
    }

    #[test]
    fn a_failed_fetch_is_not_retried_forever() {
        let mut cache = ArtCache::disabled();
        cache.should_fetch("u1");
        cache.mark_failed("u1");
        assert!(
            !cache.should_fetch("u1"),
            "a dead URL must not be retried on every tick"
        );
    }

    #[test]
    fn a_zero_sized_area_is_skipped_without_panicking() {
        use ratatui::layout::Rect;
        assert!(!should_draw(Rect::new(0, 0, 0, 0)));
        assert!(
            !should_draw(Rect::new(0, 0, 4, 2)),
            "too small to be legible"
        );
        assert!(should_draw(Rect::new(0, 0, 20, 10)));
    }

    #[test]
    fn tmux_is_detected_from_either_signal() {
        // Both matter: TMUX is unset when ssh-ing into a session, and TERM can
        // be overridden to something non-tmux inside one.
        assert!(
            in_tmux() || (std::env::var_os("TMUX").is_none() && !term_is_tmux()),
            "detection must agree with the environment it reads"
        );
    }

    fn term_is_tmux() -> bool {
        std::env::var("TERM").is_ok_and(|t| t.starts_with("tmux"))
    }

    #[test]
    fn a_fallback_picker_still_reports_enabled_so_halfblocks_render() {
        // The tmux path must not silently disable art: halfblocks work anywhere.
        let c = ArtCache::disabled();
        assert!(
            !c.is_enabled(),
            "an explicitly disabled cache stays disabled"
        );
    }

    fn big(n: u8) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(200, 200, image::Rgb([n, n, n])))
    }

    /// A solid colour encodes to spaces in the halfblocks protocol (upper and
    /// lower halves are identical), so painting tests need real variation.
    fn gradient() -> image::DynamicImage {
        let mut img = image::RgbImage::new(200, 200);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([x as u8, y as u8, (x + y) as u8]);
        }
        image::DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn the_row_cache_is_bounded_and_drops_the_oldest_cover() {
        let mut c = ArtCache::for_test();
        for i in 0..(ArtCache::MAX_ROWS + 5) {
            c.insert_row(&format!("r{i}"), tiny(i as u8));
        }
        assert_eq!(c.row_len(), ArtCache::MAX_ROWS);
        assert!(c.get_row("r0").is_none(), "the oldest cover is evicted");
        assert!(
            c.get_row(&format!("r{}", ArtCache::MAX_ROWS)).is_some(),
            "the newest cover survives"
        );
    }

    #[test]
    fn a_row_insert_cannot_evict_now_playing_art() {
        let mut c = ArtCache::for_test();
        for i in 0..ArtCache::MAX_IMAGES {
            c.insert(&format!("np{i}"), tiny(i as u8));
        }
        for i in 0..(ArtCache::MAX_ROWS + 1) {
            c.insert_row(&format!("r{i}"), tiny(i as u8));
        }
        assert_eq!(c.len(), ArtCache::MAX_IMAGES);
        assert!(c.get("np0").is_some());
        assert_eq!(c.row_len(), ArtCache::MAX_ROWS);
    }

    #[test]
    fn row_fetches_are_requested_once_and_failures_are_not_retried() {
        let mut c = ArtCache::disabled();
        assert!(c.should_fetch_row("r1"), "first sighting fetches");
        assert!(!c.should_fetch_row("r1"), "already in flight or cached");
        c.mark_row_failed("r1");
        assert!(
            !c.should_fetch_row("r1"),
            "a dead cover URL must not be retried every tick"
        );
    }

    #[test]
    fn a_disabled_row_cache_stores_nothing() {
        let mut c = ArtCache::disabled();
        c.insert_row("r1", tiny(1));
        assert_eq!(c.row_len(), 0);
        assert!(c.get_row("r1").is_none());
    }

    #[test]
    fn draw_row_is_a_no_op_when_disabled_or_given_no_space() {
        // The list calls it for every row with a URL; a tiny terminal can hand
        // it a zero-width rect. Neither may panic or paint anything.
        use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
        let mut c = ArtCache::for_test();
        c.insert_row("r1", tiny(1));
        let mut terminal = Terminal::new(TestBackend::new(4, 2)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(Paragraph::new("TEST"), area);
                draw_row(f, area, Some("r1"), &mut ArtCache::disabled());
                draw_row(f, Rect::new(0, 0, 0, 2), Some("r1"), &mut c);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(text.trim_end(), "TEST");
    }

    #[test]
    fn row_images_are_downscaled_to_at_most_ninety_six_pixels() {
        let scaled = downscale_for_rows(big(5));
        assert!(scaled.width() <= crate::widgets::row::ROW_ART_PX);
        assert!(scaled.height() <= crate::widgets::row::ROW_ART_PX);
    }

    #[test]
    fn a_row_cover_can_be_inserted_and_drawn_in_a_four_by_two_rect() {
        let mut c = ArtCache::for_test();
        c.insert_row("r1", gradient());
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(4, 2)).unwrap();
        terminal
            .draw(|f| draw_row(f, f.area(), Some("r1"), &mut c))
            .unwrap();
        let painted = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() != " ");
        assert!(painted, "halfblocks must paint inside a 4x2 cover rect");
    }

    #[test]
    fn drawing_a_missing_row_url_is_a_no_op() {
        use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};
        let mut c = ArtCache::for_test();
        let mut terminal = Terminal::new(TestBackend::new(4, 2)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                f.render_widget(Paragraph::new("TEST"), area);
                draw_row(f, area, Some("missing"), &mut c);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(
            text.trim_end(),
            "TEST",
            "nothing may be painted for a missing URL"
        );
    }

    #[test]
    fn drawing_a_row_keeps_it_in_the_cache_for_the_scroll_back() {
        let mut c = ArtCache::for_test();
        for i in 0..ArtCache::MAX_ROWS {
            c.insert_row(&format!("r{i}"), tiny(i as u8));
        }
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(4, 2)).unwrap();
        terminal
            .draw(|f| draw_row(f, f.area(), Some("r0"), &mut c))
            .unwrap();
        c.insert_row("new", tiny(1));
        assert!(
            c.get_row("r0").is_some(),
            "a just-drawn cover must survive eviction"
        );
        assert!(
            c.get_row("r1").is_none(),
            "the least recently drawn cover is the one to drop"
        );
    }
}
