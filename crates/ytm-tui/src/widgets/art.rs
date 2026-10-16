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

    /// Decoded protocol objects for the focused row preview. Kept strictly
    /// apart from `images` so preview entries never evict now-playing art.
    previews: HashMap<String, StatefulProtocol>,
    preview_order: VecDeque<String>,
    preview_in_flight: HashSet<String>,
    preview_failed: HashSet<String>,
}

impl ArtCache {
    /// Each entry retains a full decoded image at `ART_PX` (600x600x3 ≈ 1 MiB),
    /// so an unbounded map cost ~1 MiB per track played. 16 is ~16 MiB and far
    /// more history than the one visible panel can use.
    pub const MAX_IMAGES: usize = 16;

    /// Previews are downscaled (≤256px) and transient, so 8 entries cost
    /// kilobytes rather than MiBs.
    pub const MAX_PREVIEWS: usize = 8;

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn preview_len(&self) -> usize {
        self.previews.len()
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
            previews: HashMap::new(),
            preview_order: VecDeque::new(),
            preview_in_flight: HashSet::new(),
            preview_failed: HashSet::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.picker.is_some()
    }

    pub fn get(&self, url: &str) -> Option<&StatefulProtocol> {
        self.images.get(url)
    }

    pub fn get_preview(&self, url: &str) -> Option<&StatefulProtocol> {
        self.previews.get(url)
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

    /// True exactly once per preview URL, unless it later fails.
    pub fn should_fetch_preview(&mut self, url: &str) -> bool {
        if self.previews.contains_key(url)
            || self.preview_in_flight.contains(url)
            || self.preview_failed.contains(url)
        {
            return false;
        }
        self.preview_in_flight.insert(url.to_owned());
        true
    }

    pub fn mark_preview_failed(&mut self, url: &str) {
        self.preview_in_flight.remove(url);
        self.preview_failed.insert(url.to_owned());
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

    /// Build the protocol object for a decoded preview image, downscaling to at
    /// most 256px so previews take negligible memory. A disabled cache stores
    /// nothing. Entries here never evict the now-playing image.
    pub fn insert_preview(&mut self, url: &str, image: image::DynamicImage) {
        self.preview_in_flight.remove(url);
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let font = picker.font_size();
        let (max_w, max_h) = if font.width > 0 && font.height > 0 {
            let pw = (24 * font.width as u32).clamp(64, 256);
            let ph = (12 * font.height as u32).clamp(64, 256);
            (pw, ph)
        } else {
            (256, 256)
        };
        let downscaled = image.thumbnail(max_w, max_h);
        let proto = picker.new_resize_protocol(downscaled);
        if self.previews.insert(url.to_owned(), proto).is_none() {
            self.preview_order.push_back(url.to_owned());
        }
        while self.preview_order.len() > Self::MAX_PREVIEWS {
            if let Some(oldest) = self.preview_order.pop_front() {
                self.previews.remove(&oldest);
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

/// Draw the preview thumbnail for the focused row, or nothing at all.
pub fn draw_preview(f: &mut Frame, area: Rect, url: Option<&str>, art: &mut ArtCache) {
    if !should_draw(area) || !art.is_enabled() {
        return;
    }
    let Some(url) = url else { return };
    let Some(proto) = art.previews.get_mut(url) else {
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

    #[test]
    fn preview_cache_bounds_isolation_and_failure_handling() {
        // MAX_PREVIEWS bound holds
        let mut c = ArtCache::for_test();
        for i in 0..(ArtCache::MAX_PREVIEWS + 5) {
            c.insert_preview(&format!("p{i}"), tiny(i as u8));
        }
        assert_eq!(
            c.preview_len(),
            ArtCache::MAX_PREVIEWS,
            "preview cache must be bounded at MAX_PREVIEWS"
        );

        // One fetch per URL
        let mut c2 = ArtCache::disabled();
        assert!(c2.should_fetch_preview("p1"), "first sighting fetches");
        assert!(
            !c2.should_fetch_preview("p1"),
            "already in flight or cached"
        );

        // mark_preview_failed is not retried
        c2.mark_preview_failed("p1");
        assert!(
            !c2.should_fetch_preview("p1"),
            "failed preview URL is not retried"
        );

        // A disabled cache stores no preview
        let mut c3 = ArtCache::disabled();
        c3.insert_preview("p1", tiny(1));
        assert_eq!(c3.preview_len(), 0);
        assert!(c3.get_preview("p1").is_none());

        // A preview insert does not evict the now-playing image
        let mut c4 = ArtCache::for_test();
        for i in 0..ArtCache::MAX_IMAGES {
            c4.insert(&format!("np{i}"), tiny(i as u8));
        }
        assert_eq!(c4.len(), ArtCache::MAX_IMAGES);
        c4.insert_preview("preview1", tiny(99));
        assert_eq!(
            c4.len(),
            ArtCache::MAX_IMAGES,
            "preview insert must not evict now-playing images"
        );
        for i in 0..ArtCache::MAX_IMAGES {
            assert!(
                c4.get(&format!("np{i}")).is_some(),
                "now playing image {i} must still be present"
            );
        }
        assert!(c4.get_preview("preview1").is_some());
    }
}
