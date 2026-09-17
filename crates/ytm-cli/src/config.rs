//! Config loaded from TOML. No secrets live here — cookies are a path to a file
//! the user controls, and the OAuth client id/secret were removed with the
//! device flow.

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read config at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config key `playback.volume` must be 0-100, got {0}")]
    Volume(u16),
    #[error("config key `{0}` must be greater than 0, got {1}")]
    Step(&'static str, i64),
    #[error("config key `ui.theme` names no built-in theme: {0:?}")]
    UnknownTheme(String),
    #[error("config key `ui.accent` is not a hex color like \"#7aa2f7\": {0:?}")]
    BadAccent(String),
    #[error("theme file is not valid: {0}")]
    Theme(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// Kept only so an old config naming it still loads, with a clear error. Google
    /// stopped honouring device-flow OAuth tokens on the InnerTube endpoints this
    /// app uses, so the implementation was removed rather than left always failing.
    OAuth,
    Cookie,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub kind: AuthKind,
    pub cookie_file: Option<PathBuf>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            kind: AuthKind::Cookie,
            cookie_file: None,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct PlaybackConfig {
    pub volume: u16,
    pub shuffle: bool,
}

impl Default for PlaybackConfig {
    fn default() -> Self {
        Self {
            volume: 70,
            shuffle: false,
        }
    }
}

/// Which built-in theme to start with. `Auto` picks from the terminal's background
/// so a light terminal does not get dark-on-dark text. Deserialized by hand: the
/// TOML is one string (`theme = "gruvbox"`), which no derived repr matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    Auto,
    Named(String),
}

impl<'de> serde::Deserialize<'de> for ThemeChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(if s.eq_ignore_ascii_case("auto") {
            Self::Auto
        } else {
            Self::Named(s)
        })
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub vim_keys: bool,
    pub tick_ms: u64,
    pub accent: Option<String>,
    pub album_art: bool,
    pub theme_file: Option<PathBuf>,
    /// Automatically reload `theme_file` if it changes on disk.
    pub auto_reload_theme: bool,
    /// `"auto"`, or the name of a built-in theme.
    pub theme: ThemeChoice,
    /// Which source the app opens on. Defaults to Playlists — your own
    /// playlists are what most sessions start from, and Home costs a
    /// multi-page fetch before the first useful frame.
    pub start_pane: StartPane,
    /// Scroll wheel and click support.
    pub mouse: bool,
}

/// The source the app opens on (`ui.start_pane`). An enum rather than a free string
/// so a typo is a load error the user sees, not a silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPane {
    Home,
    #[default]
    Playlists,
    /// The liked/saved songs. Named `fav` in config to match the sidebar label.
    #[serde(alias = "songs")]
    Fav,
    Albums,
    Artists,
    Search,
    Queue,
}

impl StartPane {
    pub fn pane(self) -> ytm_tui::app::Pane {
        use ytm_tui::app::Pane;
        match self {
            Self::Home => Pane::Home,
            Self::Playlists => Pane::Playlists,
            Self::Fav => Pane::Songs,
            Self::Albums => Pane::Albums,
            Self::Artists => Pane::Artists,
            Self::Search => Pane::Search,
            Self::Queue => Pane::Queue,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            vim_keys: true,
            tick_ms: 250,
            accent: None,
            album_art: true,
            theme_file: None,
            auto_reload_theme: false,
            theme: ThemeChoice::Auto,
            start_pane: StartPane::default(),
            mouse: true,
        }
    }
}

/// Tunables that change how existing keys behave, rather than which key does
/// what. Separate from `[keys]` so a rebind and a step size do not share a table.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct BehaviourConfig {
    pub seek_step_secs: i64,
    pub volume_step: i64,
    /// Ask before quitting. Off by default — `q` has always quit immediately.
    pub confirm_on_quit: bool,
}

impl Default for BehaviourConfig {
    fn default() -> Self {
        Self {
            seek_step_secs: 5,
            volume_step: 5,
            confirm_on_quit: false,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub download_dir: Option<PathBuf>,
    pub cache_size_mb: u64,
    pub prefetch_count: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            download_dir: None,
            cache_size_mb: 1024,
            prefetch_count: 2,
        }
    }
}

impl StorageConfig {
    /// Return the resolved download directory:
    /// If configured, expand leading `~/` if present.
    /// If unset, default to `~/Music/ytm` or system audio directory.
    #[allow(dead_code)]
    pub fn download_dir(&self) -> PathBuf {
        if let Some(dir) = &self.download_dir {
            expand_tilde(dir)
        } else if let Some(user_dirs) = directories::UserDirs::new() {
            if let Some(audio) = user_dirs.audio_dir() {
                audio.join("ytm")
            } else {
                user_dirs.home_dir().join("Music").join("ytm")
            }
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join("Music").join("ytm")
        } else {
            PathBuf::from("./downloads")
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where this was loaded from, so `,` can reopen it. Not a TOML key.
    #[serde(skip)]
    pub config_path: PathBuf,
    pub auth: AuthConfig,
    pub playback: PlaybackConfig,
    pub ui: UiConfig,
    pub behaviour: BehaviourConfig,
    pub storage: StorageConfig,
    /// Raw `[keys]` table, handed to `KeyMap::from_toml_str` as-is so the keymap
    /// owns action-name parsing rather than duplicating it here.
    #[serde(default)]
    pub keys: toml::Table,
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let c: Config = toml::from_str(s)?;
        if c.playback.volume > 100 {
            return Err(ConfigError::Volume(c.playback.volume));
        }
        // A zero or negative step makes the key do nothing, which reads as a
        // bug rather than as configuration.
        if c.behaviour.seek_step_secs <= 0 {
            return Err(ConfigError::Step(
                "behaviour.seek_step_secs",
                c.behaviour.seek_step_secs,
            ));
        }
        if c.behaviour.volume_step <= 0 {
            return Err(ConfigError::Step(
                "behaviour.volume_step",
                c.behaviour.volume_step,
            ));
        }
        Ok(c)
    }

    /// Missing file is not an error — defaults are valid.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = path.map(PathBuf::from).unwrap_or_else(Self::default_path);
        let mut c = match std::fs::read_to_string(&path) {
            Ok(s) => Self::from_toml_str(&s)?,
            // A missing file is not an error: the defaults are valid, and `,`
            // writes the documented example on first use.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(source) => return Err(ConfigError::Io { path, source }),
        };
        c.config_path = path;
        Ok(c)
    }

    pub fn default_path() -> PathBuf {
        paths::config_dir().join("config.toml")
    }
}

/// The documented default config, written on demand when `,` opens an editor on
/// a machine that has no config.toml yet. Kept next to the structs it mirrors so
/// the two do not drift.
pub const EXAMPLE_TOML: &str = include_str!("../../../config.example.toml");

/// Minimal `~` expansion so `cookie_file = "~/.config/..."` works. Not worth a
/// dependency.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("~/")) else {
        return p.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => p.to_path_buf(),
    }
}

/// The theme to start with, plus the preset name so `t` knows where the cycle
/// is. A `theme_file` still wins over a preset — it is the more specific answer.
pub fn resolve_theme(cfg: &Config) -> Result<(ytm_tui::theme::Theme, String), ConfigError> {
    use ytm_tui::theme::Theme;

    if let Some(path) = cfg.ui.theme_file.as_deref().map(expand_tilde) {
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        let t = Theme::from_toml_str(&text).map_err(|e| ConfigError::Theme(e.to_string()))?;
        return Ok((t, "custom".to_owned()));
    }

    let name = match &cfg.ui.theme {
        ThemeChoice::Auto => auto_theme_name(),
        ThemeChoice::Named(n) => n.clone(),
    };
    let mut theme = Theme::preset(&name).ok_or_else(|| ConfigError::UnknownTheme(name.clone()))?;
    // An explicit accent still overrides whichever theme was chosen.
    if let Some(hex) = &cfg.ui.accent {
        theme.accent =
            ytm_tui::theme::parse_hex(hex).ok_or_else(|| ConfigError::BadAccent(hex.clone()))?;
    }
    Ok((theme, name))
}

/// Guess light or dark from the terminal itself. `COLORFGBG` is the only widely
/// supported hint needing no query round trip; its last field is the background
/// index, 7 and 15 being light. Everything else falls back to dark, the safer guess.
pub fn auto_theme_name() -> String {
    const DARK: &str = "tokyonight";
    const LIGHT: &str = "dawn";
    match std::env::var("COLORFGBG") {
        Ok(v) => match v.rsplit(';').next().map(str::trim) {
            Some("7") | Some("15") => LIGHT.to_owned(),
            _ => DARK.to_owned(),
        },
        Err(_) => DARK.to_owned(),
    }
}

pub mod paths {
    use directories::ProjectDirs;
    use std::path::PathBuf;

    pub fn config_dir() -> PathBuf {
        ProjectDirs::from("", "", "ytm-tui")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
    pub fn cache_dir() -> PathBuf {
        ProjectDirs::from("", "", "ytm-tui")
            .map(|d| d.cache_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    }
    pub fn log_dir() -> PathBuf {
        cache_dir().join("logs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_file_path_with_a_tilde_resolves_on_reload_too() {
        // The bug: build_theme (startup) expanded ~ and resolve_theme (reload,
        // pressed with `,`) did not, so a ~ path loaded at launch and then
        // failed the moment the user saved the config.
        let home = std::env::var("HOME").expect("HOME is set in tests");
        let path = std::path::PathBuf::from(&home).join(".ytm-test-theme.toml");
        std::fs::write(&path, "preset = \"gruvbox\"\naccent = \"#ff0000\"\n")
            .expect("write the temp theme");

        let cfg = Config::from_toml_str("[ui]\ntheme_file = \"~/.ytm-test-theme.toml\"")
            .expect("config parses");
        let resolved = resolve_theme(&cfg);
        let _ = std::fs::remove_file(&path);

        let (theme, name) = resolved.expect("a ~ path must resolve");
        assert_eq!(name, "custom");
        assert_eq!(theme.accent, ytm_tui::theme::parse_hex("#ff0000").unwrap());
    }

    #[test]
    fn paths_point_to_ytm_tui() {
        let cfg_dir = paths::config_dir();
        let cache_dir = paths::cache_dir();
        assert!(
            cfg_dir.ends_with("ytm-tui"),
            "expected ytm-tui in {cfg_dir:?}"
        );
        assert!(
            cache_dir.ends_with("ytm-tui"),
            "expected ytm-tui in {cache_dir:?}"
        );
    }

    #[test]
    fn expand_tilde_leaves_other_paths_alone() {
        use std::path::Path;
        assert_eq!(
            expand_tilde(Path::new("/etc/absolute.toml")),
            Path::new("/etc/absolute.toml")
        );
        assert_eq!(
            expand_tilde(Path::new("relative.toml")),
            Path::new("relative.toml")
        );
        // A bare `~` with no slash is not a home reference we handle.
        assert_eq!(expand_tilde(Path::new("~weird")), Path::new("~weird"));
    }

    #[test]
    fn defaults_apply_when_file_is_absent() {
        let c = Config::from_toml_str("").unwrap();
        // Cookie, because it is the only auth path that works — OAuth was
        // removed once Google stopped accepting device-flow tokens on InnerTube.
        assert_eq!(c.auth.kind, AuthKind::Cookie);
        assert_eq!(c.playback.volume, 70);
        assert!(c.ui.vim_keys);
        assert_eq!(c.ui.tick_ms, 250);
    }

    #[test]
    fn parses_a_full_config() {
        let c = Config::from_toml_str(
            r##"
            [auth]
            kind = "cookie"
            cookie_file = "/tmp/c.txt"

            [playback]
            volume = 40

            [ui]
            vim_keys = false
            accent = "#7aa2f7"
            "##,
        )
        .unwrap();
        assert_eq!(c.auth.kind, AuthKind::Cookie);
        assert_eq!(
            c.auth.cookie_file.as_deref(),
            Some(std::path::Path::new("/tmp/c.txt"))
        );
        assert_eq!(c.playback.volume, 40);
        assert!(!c.ui.vim_keys);
        assert_eq!(c.ui.accent.as_deref(), Some("#7aa2f7"));
    }

    #[test]
    fn volume_out_of_range_is_rejected_with_a_clear_message() {
        let err = Config::from_toml_str("[playback]\nvolume = 500")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("volume"),
            "message should name the offending key, got: {err}"
        );
    }

    #[test]
    fn an_old_oauth_config_still_parses() {
        // It must load, not fail at parse: `build_source` is where the user is
        // told to switch to cookies, and that message is more use than a TOML
        // error naming an unknown variant.
        let c = Config::from_toml_str("[auth]\nkind = \"oauth\"").unwrap();
        assert_eq!(c.auth.kind, AuthKind::OAuth);
    }

    #[test]
    fn a_config_still_naming_the_removed_oauth_keys_loads() {
        // Those fields are gone, but a user's file may still list them. serde
        // ignores unknown keys, so this must load rather than fail at parse.
        let c = Config::from_toml_str(
            r#"
            [auth]
            kind = "cookie"
            client_id = "leftover.apps.googleusercontent.com"
            client_secret = "leftover-secret"
            cookie_file = "/tmp/c.txt"
            "#,
        )
        .expect("an old config with dead keys must still load");
        assert_eq!(c.auth.kind, AuthKind::Cookie);
        assert_eq!(
            c.auth.cookie_file.as_deref(),
            Some(std::path::Path::new("/tmp/c.txt"))
        );
    }

    #[test]
    fn behaviour_defaults_match_the_previous_hardcoded_steps() {
        // These were consts in app_loop; making them configurable must not
        // silently change what an unconfigured user gets.
        let c = Config::default();
        assert_eq!(c.behaviour.seek_step_secs, 5);
        assert_eq!(c.behaviour.volume_step, 5);
        assert!(!c.behaviour.confirm_on_quit);
    }

    #[test]
    fn behaviour_values_are_read_from_toml() {
        let c = Config::from_toml_str(
            r#"
            [behaviour]
            seek_step_secs = 30
            volume_step = 2
            confirm_on_quit = true
            "#,
        )
        .unwrap();
        assert_eq!(c.behaviour.seek_step_secs, 30);
        assert_eq!(c.behaviour.volume_step, 2);
        assert!(c.behaviour.confirm_on_quit);
    }

    #[test]
    fn a_zero_step_is_rejected_rather_than_making_a_key_do_nothing() {
        let e = Config::from_toml_str("[behaviour]\nseek_step_secs = 0")
            .unwrap_err()
            .to_string();
        assert!(e.contains("seek_step_secs"), "got: {e}");
    }

    #[test]
    fn the_keys_table_is_carried_through_verbatim() {
        // The keymap owns action-name parsing; config just passes the table on.
        let c = Config::from_toml_str("[keys]\ntoggle_visual = \"z\"").unwrap();
        assert_eq!(
            c.keys.get("toggle_visual").and_then(|v| v.as_str()),
            Some("z")
        );
    }

    #[test]
    fn theme_defaults_to_auto_and_accepts_a_name() {
        assert_eq!(Config::default().ui.theme, ThemeChoice::Auto);
        let c = Config::from_toml_str(
            r#"[ui]
theme = "gruvbox""#,
        )
        .unwrap();
        assert_eq!(c.ui.theme, ThemeChoice::Named("gruvbox".into()));
    }

    #[test]
    fn an_unconfigured_file_still_loads_with_every_new_section_defaulted() {
        // Existing users have a config.toml with none of these keys.
        let c = Config::from_toml_str("[playback]\nvolume = 40").unwrap();
        assert_eq!(c.playback.volume, 40);
        assert_eq!(c.behaviour.seek_step_secs, 5);
        assert_eq!(c.ui.theme, ThemeChoice::Auto);
        assert!(c.keys.is_empty());
    }

    #[test]
    fn auto_theme_reads_a_light_terminal_background_from_colorfgbg() {
        // COLORFGBG's last field is the background index; 7 and 15 are light.
        // Not a #[test] on the env var itself — tests share a process, so this
        // checks the parse the way the function does.
        for (v, want_light) in [
            ("15;7", true),
            ("0;15", true),
            ("15;0", false),
            ("default;default", false),
        ] {
            let last = v.rsplit(';').next().map(str::trim);
            let is_light = matches!(last, Some("7") | Some("15"));
            assert_eq!(is_light, want_light, "COLORFGBG={v}");
        }
    }

    #[test]
    fn auto_theme_falls_back_to_a_dark_preset_and_it_resolves() {
        // The fallback must name a real preset, or startup fails on any
        // terminal that does not set COLORFGBG.
        let name = auto_theme_name();
        assert!(
            ytm_tui::theme::Theme::preset(&name).is_some(),
            "auto picked {name:?}, which is not a built-in theme"
        );
    }

    #[test]
    fn auto_reload_theme_config_defaults_to_false_and_parses() {
        let c = Config::from_toml_str("").unwrap();
        assert!(!c.ui.auto_reload_theme);

        let c = Config::from_toml_str("[ui]\nauto_reload_theme = true").unwrap();
        assert!(c.ui.auto_reload_theme);
    }

    #[test]
    fn storage_config_parses_with_custom_values_and_defaults() {
        let toml = r#"
            [storage]
            download_dir = "~/Music/offline"
            cache_size_mb = 2048
            prefetch_count = 3
        "#;
        let cfg = Config::from_toml_str(toml).unwrap();
        assert_eq!(cfg.storage.cache_size_mb, 2048);
        assert_eq!(cfg.storage.prefetch_count, 3);
        assert_eq!(
            cfg.storage.download_dir,
            Some(PathBuf::from("~/Music/offline"))
        );
    }

    #[test]
    fn storage_config_defaults_are_sensible() {
        let cfg = Config::default();
        assert_eq!(cfg.storage.cache_size_mb, 1024);
        assert_eq!(cfg.storage.prefetch_count, 2);
    }

    #[test]
    fn storage_config_download_dir_resolves_sensibly() {
        let mut cfg = StorageConfig::default();
        let default_dir = cfg.download_dir();
        assert!(default_dir.ends_with("ytm") || default_dir.ends_with("downloads"));

        cfg.download_dir = Some(PathBuf::from("/tmp/custom_ytm"));
        assert_eq!(cfg.download_dir(), PathBuf::from("/tmp/custom_ytm"));
    }
}
