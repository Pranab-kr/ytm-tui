//! Persistent on-disk cache for album and row-cover bytes.
//!
//! The in-memory caches in `ytm_tui::widgets::art` die with the process, so
//! every launch re-downloaded the same covers. This keeps the raw bytes on disk
//! under a hashed filename, bounded by a hard cap with LRU eviction. It is a
//! pure cache: every failure is a miss, never an error the UI has to handle.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct DiskArtCache {
    state: Arc<Mutex<State>>,
}

struct State {
    dir: PathBuf,
    max_bytes: u64,
    indexed: bool,
    /// Least recently used first. Keys are file stems (the URL hash).
    order: VecDeque<String>,
    sizes: HashMap<String, u64>,
    total: u64,
}

impl DiskArtCache {
    pub fn with_limit(dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                dir,
                max_bytes,
                indexed: false,
                order: VecDeque::new(),
                sizes: HashMap::new(),
                total: 0,
            })),
        }
    }

    pub fn get(&self, url: &str) -> Option<Vec<u8>> {
        let stem = file_stem(url);
        let mut state = self.state.lock().ok()?;
        ensure_indexed(&mut state);

        let size = *state.sizes.get(&stem)?;
        let path = state.dir.join(format!("{stem}.img"));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                // The index can outlive a file deleted outside the cache. Forget
                // the entry so a later put starts from the truth.
                state.sizes.remove(&stem);
                state.total = state.total.saturating_sub(size);
                state.order.retain(|key| key != &stem);
                return None;
            }
        };

        // A file changed outside the cache (or a partial write) is accounted for
        // at its real size rather than the indexed one.
        let actual = bytes.len() as u64;
        if actual != size {
            state.total = state.total.saturating_sub(size).saturating_add(actual);
            state.sizes.insert(stem.clone(), actual);
        }

        // Promote: move to the back of the queue, where eviction starts from the
        // front. `retain` first keeps a duplicate from ever appearing.
        state.order.retain(|key| key != &stem);
        state.order.push_back(stem.clone());

        // Persist recency for the next process, whose LRU order comes from
        // mtimes. Failing here is harmless: this session's order is in memory.
        if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }

        Some(bytes)
    }

    pub fn put(&self, url: &str, bytes: &[u8]) -> io::Result<()> {
        let stem = file_stem(url);
        let size = bytes.len() as u64;
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("art cache lock poisoned"))?;

        // A single image that cannot fit is skipped rather than evicting
        // everything else to make room for something the cap cannot hold.
        if size > state.max_bytes {
            return Ok(());
        }
        ensure_indexed(&mut state);
        std::fs::create_dir_all(&state.dir)?;

        // Same directory, so the rename is atomic: a reader sees either the old
        // file or the complete new one, never a half-written image.
        let tmp = state.dir.join(format!(
            "{stem}.{}.{}.tmp",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(e) = std::fs::write(&tmp, bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        let path = state.dir.join(format!("{stem}.img"));
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }

        // Replace rather than add: re-fetching a URL must not count its bytes
        // twice or leave two slots in the queue for one file.
        if let Some(old) = state.sizes.insert(stem.clone(), size) {
            state.total = state.total.saturating_sub(old);
            state.order.retain(|key| key != &stem);
        }
        state.total = state.total.saturating_add(size);
        state.order.push_back(stem);

        evict_to_limit(&mut state);
        Ok(())
    }

    pub fn remove(&self, url: &str) {
        let stem = file_stem(url);
        // A poisoned lock means another task panicked. There is nothing safe to
        // do with the index, and this is only a cache, so skip the cleanup.
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        ensure_indexed(&mut state);
        if let Some(size) = state.sizes.remove(&stem) {
            state.total = state.total.saturating_sub(size);
        }
        state.order.retain(|key| key != &stem);
        let _ = std::fs::remove_file(state.dir.join(format!("{stem}.img")));
    }
}

/// Temp-file names only have to be unique within the process; the pid and this
/// counter are enough, and neither can collide with a `.img` index entry.
static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// Build the on-disk index once per cache instance. Files are ordered by mtime,
/// oldest first, so the LRU order survives a restart. Only top-level `.img`
/// files whose stem looks like ours count; temp files and subdirectories are
/// ignored.
fn ensure_indexed(state: &mut State) {
    if state.indexed {
        return;
    }
    state.indexed = true;

    let Ok(entries) = std::fs::read_dir(&state.dir) else {
        return;
    };
    let mut found: Vec<(String, u64, std::time::SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // A crash between `write` and `rename` leaves a temp file that is not
        // part of the index. Sweep it here so a run of failed writes cannot
        // grow the directory beyond the cap indefinitely.
        if path.extension().and_then(|e| e.to_str()) == Some("tmp") {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|e| e.to_str()) != Some("img") {
            continue;
        }
        if stem.len() != 32 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        // `metadata` follows symlinks, so this also rejects a directory named
        // `<hash>.img` without reading it.
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        found.push((stem.to_owned(), meta.len(), mtime));
    }
    found.sort_by_key(|entry| entry.2);

    for (stem, size, _) in found {
        if state.sizes.insert(stem.clone(), size).is_none() {
            state.total = state.total.saturating_add(size);
            state.order.push_back(stem);
        }
    }

    // The cap can have been lowered since the files were written, or another
    // process may have exceeded it. Trim on first use so the guarantee holds
    // from the start of the session, not only after the first put.
    evict_to_limit(state);
}

/// Drop the oldest entries until the directory fits. An unlink failure is not
/// fatal: the entry leaves the index either way, so a later put can retry the
/// same accounting without the cache appearing to grow without bound.
fn evict_to_limit(state: &mut State) {
    while state.total > state.max_bytes {
        let Some(stem) = state.order.pop_front() else {
            break;
        };
        let Some(size) = state.sizes.remove(&stem) else {
            continue;
        };
        state.total = state.total.saturating_sub(size);
        let _ = std::fs::remove_file(state.dir.join(format!("{stem}.img")));
    }
}

/// A 128-bit FNV-1a hash of the URL, as 32 lowercase hex digits. The filename
/// never contains the URL itself — no separators, no query strings, and no
/// accidental account identifiers on disk.
fn file_stem(url: &str) -> String {
    const OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    let mut hash = OFFSET_BASIS;
    for byte in url.as_bytes() {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique directory under the system temp dir, removed on drop. A guard
    /// rather than a manual cleanup call so a panicking assertion cannot leave
    /// test images behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "ytm-tui-art-disk-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create the temp dir");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn img_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|ext| ext == "img"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_mtime(path: &Path, secs: u64) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open the cached file");
        file.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .expect("set the cached file's mtime");
    }

    /// The shipped default, so the tests exercise the same cap a user gets.
    const TEST_CAP: u64 = 128 * 1024 * 1024;

    fn default_cache(dir: &TempDir) -> DiskArtCache {
        DiskArtCache::with_limit(dir.path(), TEST_CAP)
    }

    #[test]
    fn a_missing_url_is_a_miss_not_an_error() {
        let dir = TempDir::new("miss");
        let cache = default_cache(&dir);
        assert!(cache.get("https://example.com/missing.jpg").is_none());
    }

    #[test]
    fn put_then_get_round_trips_the_exact_bytes() {
        let dir = TempDir::new("round-trip");
        let cache = default_cache(&dir);
        let bytes = b"\x89PNG\r\n\x1a\nnot really a png";
        cache.put("https://example.com/a.jpg", bytes).unwrap();
        assert_eq!(cache.get("https://example.com/a.jpg").unwrap(), bytes);
    }

    #[test]
    fn replacing_a_url_updates_the_total_without_double_counting() {
        let dir = TempDir::new("replace");
        let cache = DiskArtCache::with_limit(dir.path(), 1024);
        cache.put("u", b"aaaa").unwrap();
        cache.put("u", b"bb").unwrap();

        assert_eq!(cache.get("u").unwrap(), b"bb");
        let state = cache.state.lock().unwrap();
        assert_eq!(state.sizes.len(), 1, "one URL is one index entry");
        assert_eq!(state.total, 2, "the old size must not still be counted");
    }

    #[test]
    fn eviction_drops_the_least_recently_used_url_to_stay_under_the_cap() {
        let dir = TempDir::new("lru");
        let cache = DiskArtCache::with_limit(dir.path(), 10);
        cache.put("a", b"aaaa").unwrap();
        cache.put("b", b"bbbb").unwrap();
        // Reading `a` makes `b` the least recently used, even though it is newer.
        assert_eq!(cache.get("a").unwrap(), b"aaaa");
        cache.put("c", b"cccc").unwrap();

        assert_eq!(cache.get("a").unwrap(), b"aaaa", "a was just used");
        assert!(cache.get("b").is_none(), "b is the least recently used");
        assert_eq!(cache.get("c").unwrap(), b"cccc");
        assert_eq!(cache.state.lock().unwrap().total, 8);
    }

    #[test]
    fn an_image_larger_than_the_whole_cap_is_not_cached() {
        let dir = TempDir::new("too-big");
        let cache = DiskArtCache::with_limit(dir.path(), 3);
        cache.put("huge", b"1234").unwrap();

        assert!(cache.get("huge").is_none());
        assert!(img_files(&dir.path()).is_empty(), "nothing may be written");
        assert_eq!(cache.state.lock().unwrap().total, 0);
    }

    #[test]
    fn different_urls_get_different_files() {
        let dir = TempDir::new("distinct");
        let cache = default_cache(&dir);
        cache.put("https://example.com/a.jpg", b"a").unwrap();
        cache.put("https://example.com/b.jpg", b"b").unwrap();

        let files = img_files(&dir.path());
        assert_eq!(files.len(), 2);
        assert_ne!(files[0], files[1]);
    }

    #[test]
    fn a_reopened_cache_still_serves_the_bytes_from_disk() {
        let dir = TempDir::new("reopen");
        {
            let cache = default_cache(&dir);
            cache
                .put("https://example.com/a.jpg", b"cover bytes")
                .unwrap();
        }
        let reopened = default_cache(&dir);
        assert_eq!(
            reopened.get("https://example.com/a.jpg").unwrap(),
            b"cover bytes",
            "a later launch must not need the network for a known cover"
        );
        assert_eq!(img_files(&dir.path()).len(), 1);
    }

    #[test]
    fn the_index_loads_files_in_oldest_mtime_order() {
        // The restart equivalent of the in-memory LRU: whichever file was used
        // longest ago is the first to go when the cap is tight.
        let dir = TempDir::new("mtime-order");
        {
            let cache = DiskArtCache::with_limit(dir.path(), 100);
            cache.put("old", b"aaaa").unwrap();
            cache.put("new", b"bbbb").unwrap();
            set_mtime(&dir.path().join(format!("{}.img", file_stem("old"))), 1_000);
            set_mtime(&dir.path().join(format!("{}.img", file_stem("new"))), 2_000);
        }

        let reopened = DiskArtCache::with_limit(dir.path(), 6);
        assert!(
            reopened.get("old").is_none(),
            "the oldest file is evicted first when the cap shrinks"
        );
        assert_eq!(reopened.get("new").unwrap(), b"bbbb");
    }

    #[test]
    fn index_loading_ignores_temp_files_and_subdirectories() {
        let dir = TempDir::new("ignore");
        std::fs::write(dir.path().join("leftover.tmp"), b"junk").unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(
            dir.path()
                .join("nested")
                .join(format!("{}.img", file_stem("nested"))),
            b"junk",
        )
        .unwrap();

        let cache = default_cache(&dir);
        assert!(cache.get("anything").is_none());
        assert!(
            !dir.path().join("leftover.tmp").exists(),
            "a crash-orphaned temp file must not escape the cap forever"
        );
        let state = cache.state.lock().unwrap();
        assert_eq!(state.total, 0, "only top-level .img files are ours");
        assert!(state.sizes.is_empty());
    }

    #[test]
    fn remove_deletes_the_file_and_turns_the_next_get_into_a_miss() {
        let dir = TempDir::new("remove");
        let cache = default_cache(&dir);
        cache.put("u", b"bytes").unwrap();
        assert!(cache.get("u").is_some());

        cache.remove("u");

        assert!(cache.get("u").is_none());
        assert!(img_files(&dir.path()).is_empty());
        assert_eq!(cache.state.lock().unwrap().total, 0);
    }

    #[test]
    fn the_filename_is_a_stable_32_hex_digit_hash() {
        let a = file_stem("https://example.com/a.jpg");
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, file_stem("https://example.com/a.jpg"));
        assert_ne!(a, file_stem("https://example.com/b.jpg"));
        assert_eq!(
            file_stem(""),
            "6c62272e07bb014262b821756295c58d",
            "FNV-1a 128 of the empty string is the offset basis"
        );
    }

    #[test]
    fn the_cache_can_move_between_blocking_tasks() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DiskArtCache>();
    }
}
