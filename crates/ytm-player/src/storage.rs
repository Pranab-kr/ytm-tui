//! Local audio storage management and lookahead cache.

use std::path::{Path, PathBuf};
use ytm_core::VideoId;

/// Manages local audio files: permanent offline downloads and bounded lookahead cache.
#[derive(Debug, Clone)]
pub struct AudioStorageManager {
    download_dir: PathBuf,
    cache_dir: PathBuf,
    cache_size_mb: u64,
}

impl AudioStorageManager {
    pub fn new(download_dir: PathBuf, cache_dir: PathBuf, cache_size_mb: u64) -> Self {
        Self {
            download_dir,
            cache_dir,
            cache_size_mb,
        }
    }

    pub fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn cache_size_mb(&self) -> u64 {
        self.cache_size_mb
    }

    /// Look for local audio file corresponding to `id`.
    /// Checks `download_dir` first (permanent downloads), then `cache_dir` (lookahead cache).
    pub fn find_local_audio(&self, id: &VideoId) -> Option<PathBuf> {
        for ext in [".opus", ".webm", ".m4a", ".mp3", ""] {
            let p = self.download_dir.join(format!("{}{ext}", id.as_str()));
            if p.is_file() {
                return Some(p);
            }
        }
        for ext in [".opus", ".webm", ".m4a", ".mp3", ""] {
            let p = self.cache_dir.join(format!("{}{ext}", id.as_str()));
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    /// Prune `cache_dir` using LRU (Least Recently Used) policy until total size is
    /// under `max_bytes` (targeting <= 90% of `max_bytes` per FR-QC5).
    pub fn prune_lru(&self, max_bytes: u64) -> std::io::Result<usize> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }
        let mut entries = Vec::new();
        let mut total_size: u64 = 0;

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                let meta = entry.metadata()?;
                let size = meta.len();
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let atime = meta.accessed().unwrap_or(mtime);
                let key = atime.min(mtime);
                total_size += size;
                entries.push((key, size, path));
            }
        }

        if total_size <= max_bytes {
            return Ok(0);
        }

        entries.sort_by_key(|(time, _, _)| *time);

        let target_size = (max_bytes * 9) / 10;
        let mut pruned = 0;

        for (_time, size, path) in entries {
            if total_size <= target_size && total_size <= max_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total_size = total_size.saturating_sub(size);
                pruned += 1;
            }
        }

        Ok(pruned)
    }

    /// Prune cache according to configured `cache_size_mb`.
    pub fn prune_cache(&self) -> std::io::Result<usize> {
        self.prune_lru(self.cache_size_mb * 1024 * 1024)
    }

    /// Download audio for `video_id` into `target_path` synchronously using `yt-dlp`.
    pub fn download_track_sync(
        &self,
        video_id: &VideoId,
        target_path: &Path,
        cookie_jar: Option<&Path>,
    ) -> Result<PathBuf, crate::resolver::ResolveError> {
        if let Some(parent) = target_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let url = format!("https://music.youtube.com/watch?v={video_id}");
        let mut cmd = std::process::Command::new("yt-dlp");
        cmd.args(["-f", "bestaudio", "--no-playlist", "--no-warnings", "-o"]);
        cmd.arg(target_path.as_os_str());
        if let Some(jar) = cookie_jar {
            cmd.arg("--cookies").arg(jar.as_os_str());
        }
        cmd.arg(&url);

        let out = cmd.output().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => crate::resolver::ResolveError::NotInstalled,
            _ => crate::resolver::ResolveError::Failed(e.to_string()),
        })?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("not a bot") || stderr.contains("Sign in to confirm") {
                return Err(crate::resolver::ResolveError::Failed(
                    "YouTube requested sign-in to stream or download this audio".to_owned(),
                ));
            }
            return Err(crate::resolver::ResolveError::Failed(
                stderr.lines().last().unwrap_or("unknown error").to_owned(),
            ));
        }

        Ok(target_path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_local_audio_prefers_downloads_over_cache() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("ytm-storage-test-1-{}", nanos));
        let dl_dir = tmp.join("downloads");
        let cache_dir = tmp.join("cache");
        std::fs::create_dir_all(&dl_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();

        let id = VideoId::from("abc123");
        let dl_file = dl_dir.join("abc123.opus");
        let cache_file = cache_dir.join("abc123.opus");

        std::fs::write(&cache_file, b"cached").unwrap();
        let mgr = AudioStorageManager::new(dl_dir.clone(), cache_dir.clone(), 1024);
        assert_eq!(mgr.find_local_audio(&id), Some(cache_file));

        std::fs::write(&dl_file, b"downloaded").unwrap();
        assert_eq!(mgr.find_local_audio(&id), Some(dl_file));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn lru_eviction_removes_oldest_files_when_capacity_exceeded() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("ytm-storage-test-2-{}", nanos));
        let cache_dir = tmp.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let f1 = cache_dir.join("old.opus");
        let f2 = cache_dir.join("new.opus");
        std::fs::write(&f1, vec![0u8; 600]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&f2, vec![0u8; 600]).unwrap();

        let mgr = AudioStorageManager::new(tmp.join("dl"), cache_dir, 1); // 1 MB limit
        mgr.prune_lru(800).unwrap();
        assert!(!f1.exists() || !f2.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
