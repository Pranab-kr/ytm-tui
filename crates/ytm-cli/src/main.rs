mod app_loop;
mod config;
mod logging;
mod mpris;
mod state_file;

use color_eyre::eyre::{Context, eyre};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io::{self, Stdout};
use std::sync::Arc;
use ytm_core::MusicSource;
use ytm_tui::{app::AppState, keymap::KeyMap};

/// Restores the terminal on drop, including during a panic unwind (NFR-5).
pub struct TerminalGuard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// `mouse` follows `ui.mouse`. Off is a real preference: while capture is on,
    /// the terminal hands us the clicks and drags it would otherwise use for its
    /// own text selection.
    pub fn new(mouse: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(out, EnterAlternateScreen, crossterm::cursor::Hide)?;
        if mouse {
            execute!(out, EnableMouseCapture)?;
        }
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(out))?,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    // Mouse capture off before leaving: a terminal left in capture mode ignores
    // the user's own selection and scrollback afterwards.
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

/// Put the terminal back *before* the panic message is printed, or the report
/// lands in the alternate screen and vanishes with it (NFR-5). `Drop` alone is
/// not enough: it runs after the hook.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}

/// Build whichever `MusicSource` config selects — browser cookies are the only
/// working auth path. The OAuth device flow was removed on 2026-08-31 (Google
/// stopped honouring those tokens on InnerTube); `AuthKind::OAuth` still parses.
async fn build_authenticated_source(
    cfg: &config::Config,
) -> color_eyre::Result<Arc<dyn MusicSource>> {
    use ytm_core::ytmusic::YtMusicSource;

    if cfg.auth.kind == config::AuthKind::OAuth {
        return Err(eyre!(
            "auth.kind = \"oauth\" is no longer supported — Google stopped accepting \
             device-flow tokens on YouTube Music's endpoints. Set auth.kind = \"cookie\" \
             and auth.cookie_file in {} (see the README for how to export cookies).",
            config::Config::default_path().display()
        ));
    }

    let path = cfg
        .auth
        .cookie_file
        .as_deref()
        .map(config::expand_tilde)
        .ok_or_else(|| {
            eyre!(
                "auth.cookie_file is not set in {} — see the README for how to export \
                 your cookies",
                config::Config::default_path().display()
            )
        })?;
    let source = YtMusicSource::from_cookie_file(&path)
        .await
        .with_context(|| {
            format!(
                "could not use the cookie file at {} — it may have expired; re-export it",
                path.display()
            )
        })?;
    Ok(Arc::new(source))
}

/// The TUI's source: cookies when they work, otherwise a guest session. A missing
/// or stale cookie is no longer fatal — search, queue, and playback work without
/// auth; `auth.kind = "oauth"` remains an error because that path cannot work.
async fn build_source_or_guest(cfg: &config::Config) -> color_eyre::Result<Arc<dyn MusicSource>> {
    if cfg.auth.kind == config::AuthKind::OAuth {
        return build_authenticated_source(cfg).await;
    }
    match build_authenticated_source(cfg).await {
        Ok(source) => Ok(source),
        Err(error) => {
            // The error itself, not the cookie contents.
            tracing::warn!(error = %error, "cookie auth unavailable; starting guest mode");
            Ok(Arc::new(
                ytm_core::ytmusic::YtMusicSource::unauthenticated().await?,
            ))
        }
    }
}

/// Whether the first frame should already be a guest's. Answered from config alone,
/// because the start pane and cache preload are chosen before the source exists.
/// An expired cookie is invisible here — `source_ready` catches that later.
fn guest_startup(cfg: &config::Config) -> bool {
    cfg.auth.kind == config::AuthKind::Cookie
        && cfg
            .auth
            .cookie_file
            .as_deref()
            .map(config::expand_tilde)
            .is_none_or(|path| !path.is_file())
}

/// `playback.shuffle` has to reach the queue, not just the indicator: the actor
/// owns the play order, and `AppState::shuffle` is only what the arrow draws.
/// Off is the actor's own default, so only `true` is worth a command.
fn apply_startup_shuffle(player: &impl ytm_player::player::Player, shuffle: bool) {
    if shuffle && let Err(e) = player.send(ytm_player::player::PlayerCommand::SetShuffle(true)) {
        tracing::warn!(error = %e, "could not enable shuffle at startup");
    }
}

/// `ytm` with no subcommand launches the TUI; the subcommands are the
/// non-interactive paths, which is what makes auth debuggable without a
/// terminal UI in the way.
#[derive(Debug, clap::Parser)]
#[command(name = "ytm", about = "YouTube Music in your terminal", version)]
pub struct Cli {
    /// Path to config.toml (defaults to the platform config dir)
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Print your playlists and exit
    Playlists,
    /// Write config.toml with every default and binding, then open it
    Config {
        /// Write the file and print its path without opening an editor
        #[arg(long)]
        no_edit: bool,
    },
    /// Cache maintenance
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
}

#[derive(Debug, clap::Subcommand, PartialEq, Eq)]
pub enum CacheAction {
    /// Delete all cached metadata
    Clear,
}

/// One playlist title per line — the fastest auth check there is, and scriptable.
async fn run_playlists(cfg: &config::Config) -> color_eyre::Result<()> {
    let source = build_authenticated_source(cfg).await?;
    let playlists = source.library_playlists().await?;
    if playlists.is_empty() {
        // Same trap as the TUI's empty-library hint: an expired cookie answers
        // HTTP 200 with zero rows, so silence here would read as "no playlists".
        return Err(eyre!(
            "the library came back empty — if that is wrong, the cookie or token has expired"
        ));
    }
    for p in &playlists {
        match p.track_count {
            Some(n) => println!("{}\t{} tracks", p.title, n),
            None => println!("{}", p.title),
        }
    }
    Ok(())
}

/// One top-level section of the bundled example, with the comments that
/// document it, ready to append to a file that lacks it.
struct ExampleSection {
    name: String,
    text: String,
}

/// Split `EXAMPLE_TOML` into its top-level sections. Comment lines directly above a
/// header travel with it — that is where the example says what the section is for.
/// A blank line ends the run, keeping the file's own header out of `[auth]`.
fn example_sections() -> Vec<ExampleSection> {
    let lines: Vec<&str> = config::EXAMPLE_TOML.lines().collect();

    // (header line, first line to copy) — the second walks back over comments.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if is_section_header(l) {
            let mut start = i;
            while start > 0 && lines[start - 1].trim_start().starts_with('#') {
                start -= 1;
            }
            spans.push((i, start));
        }
    }

    let mut out = Vec::with_capacity(spans.len());
    for (n, &(header, start)) in spans.iter().enumerate() {
        let end = spans.get(n + 1).map_or(lines.len(), |&(_, next)| next);
        out.push(ExampleSection {
            name: lines[header].trim().trim_matches(['[', ']']).to_owned(),
            text: lines[start..end].join("\n"),
        });
    }
    out
}

fn is_section_header(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('[') && t.ends_with(']') && !t.starts_with("[[")
}

/// The key a line defines, commented out or not — which is what separates a
/// documented default from prose.
fn toml_key_of(line: &str) -> Option<&str> {
    let t = line.trim();
    let t = t.strip_prefix('#').map_or(t, str::trim_start);
    let k = t.split('=').next()?.trim();
    (!k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(k)
}

/// The raw lines inside one section of a file, header excluded.
fn section_body<'a>(text: &'a str, section: &str) -> Vec<&'a str> {
    let header = format!("[{section}]");
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == header) else {
        return Vec::new();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| is_section_header(l))
        .map_or(lines.len(), |i| start + 1 + i);
    lines[start + 1..end].to_vec()
}

/// One setting from the example, with the comments that document it.
struct ExampleEntry {
    key: String,
    /// Those comments plus the definition, the definition commented out.
    text: String,
}

/// The settings of one example section, in the order the example lists them.
/// Commented out, because this goes into a section the user already wrote: their
/// omissions look deliberate. A wholly absent section is appended verbatim instead.
fn example_entries(section: &ExampleSection) -> Vec<ExampleEntry> {
    let header = format!("[{}]", section.name);
    let mut body = section.text.lines().skip_while(|l| l.trim() != header);
    body.next(); // the header itself

    let mut comments: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for line in body {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            comments.clear();
            continue;
        }
        match toml_key_of(line) {
            Some(key) => {
                let mut text = comments.join("\n");
                if !text.is_empty() {
                    text.push('\n');
                }
                if !trimmed.starts_with('#') {
                    text.push_str("# ");
                }
                text.push_str(line.trim_end());
                out.push(ExampleEntry {
                    key: key.to_owned(),
                    text,
                });
                comments.clear();
            }
            None if trimmed.starts_with('#') => comments.push(line.trim_end()),
            None => comments.clear(),
        }
    }
    out
}

/// Put `entries` at the end of `section`'s existing block. They have to land under
/// the header they belong to: TOML forbids a second `[ui]`, so appending to the end
/// of the file would attach them to whichever section happens to be last.
fn insert_into_section(text: &str, section: &str, entries: &[ExampleEntry]) -> Option<String> {
    let header = format!("[{section}]");
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| l.trim() == header)?;
    let mut end = lines[start + 1..]
        .iter()
        .position(|l| is_section_header(l))
        .map_or(lines.len(), |i| start + 1 + i);
    // Trailing blank lines sit between the sections, not inside this one.
    while end > start + 1 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }

    let mut out: Vec<String> = lines[..end].iter().map(|l| (*l).to_owned()).collect();
    for entry in entries {
        out.push(String::new());
        out.push(entry.text.clone());
    }
    out.extend(lines[end..].iter().map(|l| (*l).to_owned()));
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    Some(joined)
}

/// What an existing config does not mention.
struct Gaps {
    /// Sections it has no header for at all.
    absent: Vec<ExampleSection>,
    /// Settings absent from a section it does have, by section name.
    partial: Vec<(String, Vec<ExampleEntry>)>,
}

/// Compare an existing config against the bundled example. `None` means the file
/// does not parse, which is a different report. Both halves derive from
/// `EXAMPLE_TOML`, so neither can drift from it the way the README did.
fn gaps(text: &str) -> Option<Gaps> {
    let have: toml::Table = text.parse().ok()?;
    let want: toml::Table = config::EXAMPLE_TOML
        .parse()
        .expect("the bundled example parses; a test covers this");

    let mut g = Gaps {
        absent: Vec::new(),
        partial: Vec::new(),
    };
    for section in example_sections() {
        let Some(mine) = have.get(&section.name).and_then(toml::Value::as_table) else {
            if !have.contains_key(&section.name) {
                g.absent.push(section);
            }
            continue;
        };
        let Some(theirs) = want.get(&section.name).and_then(toml::Value::as_table) else {
            continue;
        };
        // A key this command already wrote is commented out, so it is absent from
        // the parsed table — matching on that alone re-appended it every run. The
        // raw lines say whether the default is already there to uncomment.
        let mentioned: Vec<&str> = section_body(text, &section.name)
            .into_iter()
            .filter_map(toml_key_of)
            .collect();
        let missing: Vec<ExampleEntry> = example_entries(&section)
            .into_iter()
            .filter(|e| {
                theirs.contains_key(&e.key)
                    && !mine.contains_key(&e.key)
                    && !mentioned.contains(&e.key.as_str())
            })
            .collect();
        if !missing.is_empty() {
            g.partial.push((section.name.clone(), missing));
        }
    }
    Some(g)
}

/// Bring an existing config up to the documented example: preserve user lines,
/// append absent sections verbatim, and add missing settings as comments under
/// their existing headers. Parse the result before writing it.
fn top_up(path: &std::path::Path) -> color_eyre::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let Some(g) = gaps(&text) else {
        println!("\nThis file is not valid TOML, so the app is running entirely on");
        println!("defaults. Repairing it is what this command is for.");
        return Ok(());
    };
    if g.absent.is_empty() && g.partial.is_empty() {
        return Ok(());
    }

    let mut topped = text;
    for (name, entries) in &g.partial {
        if let Some(t) = insert_into_section(&topped, name, entries) {
            topped = t;
        }
    }
    if !g.absent.is_empty() {
        if !topped.ends_with('\n') {
            topped.push('\n');
        }
        for section in &g.absent {
            topped.push('\n');
            topped.push_str(section.text.trim_end());
            topped.push('\n');
        }
    }

    // Should not fail — the example parses and these were the parts missing from
    // it — so say so rather than writing a file that will not load.
    if config::Config::from_toml_str(&topped).is_err() {
        println!("\nsome settings are missing from this file, but adding them would not");
        println!("parse. config.example.toml in the repo lists them all.");
        return Ok(());
    }
    std::fs::write(path, &topped)?;

    if !g.absent.is_empty() {
        let names: Vec<&str> = g.absent.iter().map(|s| s.name.as_str()).collect();
        println!(
            "\nadded at their documented defaults: [{}]",
            names.join("], [")
        );
    }
    for (name, entries) in &g.partial {
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        println!("\nadded to [{name}], commented out: {}", keys.join(", "));
    }
    Ok(())
}

/// Write the documented config and open it in `$EDITOR` (FR-U9). The file that lands
/// carries every setting and keybinding at its default, commented out, so changing
/// one is uncommenting a line. An existing file is opened as it is, never overwritten.
fn run_config(path_override: Option<&std::path::Path>, no_edit: bool) -> color_eyre::Result<()> {
    let path = path_override
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(config::Config::default_path);
    let existed = path.exists();
    if !existed {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, config::EXAMPLE_TOML)?;
    }

    println!(
        "{} {}",
        if existed {
            "config:"
        } else {
            "config written:"
        },
        path.display()
    );

    // An existing file is never overwritten, but it can be older than the settings
    // it does not mention, so it is topped up rather than merely reported on. Done
    // before the editor opens so what was added is on screen to edit.
    if existed {
        top_up(&path)?;
    }

    if no_edit {
        println!("\nEvery setting and keybinding is in that file, commented out at its");
        println!("default. Uncomment a line to change it.");
        return Ok(());
    }

    let Some(editor) = app_loop::editor_command() else {
        // Not an error: the file is written, which is the useful half. Telling
        // the user to set $EDITOR and exiting 1 would bury that.
        println!("\n$EDITOR is not set, so the file was not opened.");
        println!("Edit it directly, or set $EDITOR and run this again.");
        return Ok(());
    };

    let mut parts = editor.split_whitespace();
    let bin = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(bin)
        .args(parts)
        .arg(&path)
        .status()?;
    if !status.success() {
        return Err(eyre!("{bin} exited with {status}"));
    }
    // Validated after editing so a typo is caught here rather than surfacing as
    // a silently ignored binding later.
    match config::Config::load(Some(&path)) {
        Ok(_) => println!("config is valid"),
        Err(e) => return Err(eyre!("config.toml is not valid: {e}")),
    }
    Ok(())
}

fn run_cache_clear() -> color_eyre::Result<()> {
    let path = config::paths::cache_dir().join("cache.db");
    let cache = ytm_core::cache::Cache::open(&path)?;
    cache.clear()?;
    println!("Cache cleared ({}).", path.display());
    Ok(())
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    use clap::Parser;
    color_eyre::install()?;
    // Held for the process lifetime; dropping it loses buffered log lines.
    let _log_guard = logging::init(&config::paths::log_dir())?;
    let cli = Cli::parse();

    // `config` is dispatched before the file is read, deliberately. It is the
    // command that repairs a broken config.toml, so making it depend on a
    // loadable one locked the user out of the only tool for the job.
    if let Some(Command::Config { no_edit }) = &cli.command {
        return run_config(cli.config.as_deref(), *no_edit);
    }

    let cfg = config::Config::load(cli.config.as_deref())?;

    // Every subcommand returns an Err on failure, which `main` turns into a
    // non-zero exit — that is what makes these usable from a script.
    match &cli.command {
        Some(Command::Playlists) => return run_playlists(&cfg).await,
        Some(Command::Config { .. }) => unreachable!("handled above"),
        Some(Command::Cache { action }) => {
            return match action {
                CacheAction::Clear => run_cache_clear(),
            };
        }
        None => {}
    }
    run_tui(cfg).await
}

/// The default path: the full terminal UI.
async fn run_tui(cfg: config::Config) -> color_eyre::Result<()> {
    tracing::info!(
        auth = ?cfg.auth.kind,
        volume = cfg.playback.volume,
        vim_keys = cfg.ui.vim_keys,
        "config loaded"
    );

    // Opened before the source so the first frame can be drawn from it. A
    // corrupt file rebuilds itself; an unopenable one degrades to no cache
    // rather than blocking startup, because none of this is authoritative data.
    let cache_path = config::paths::cache_dir().join("cache.db");
    let cache = match ytm_core::cache::Cache::open(&cache_path) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, path = %cache_path.display(), "running without a cache");
            None
        }
    };

    // `state.toml` wins over `playback.volume`: the config value is the starting
    // point for a fresh install, and after that the last session's level is what
    // "remembered across runs" means.
    let state_path = state_file::default_path();
    let volume = state_file::load(&state_path)
        .volume
        .unwrap_or_else(|| cfg.playback.volume.min(100) as u8);
    let (theme, theme_name) = config::resolve_theme(&cfg)?;

    // Decided before the first frame: the start pane, the cache preload, and
    // whether yt-dlp gets cookies all depend on it, and all three happen before
    // the source's handshake returns.
    let guest = guest_startup(&cfg);

    // Fails cleanly here rather than mid-frame if libmpv is missing. yt-dlp gets the
    // same cookie file the API uses, or YouTube bot-checks every stream request. A
    // guest has none to give and runs bare, which works unless the IP is checked.
    let cookie_path = if guest {
        None
    } else {
        cfg.auth.cookie_file.as_deref().map(config::expand_tilde)
    };
    let audio_storage = ytm_player::storage::AudioStorageManager::new(
        cfg.storage.download_dir(),
        config::paths::cache_dir().join("audio"),
        cfg.storage.cache_size_mb,
    )
    .with_prefetch_count(cfg.storage.prefetch_count);
    let (player, player_events) =
        ytm_player::actor::spawn_player(volume, cookie_path.clone(), Some(audio_storage.clone()))?;
    apply_startup_shuffle(&player, cfg.playback.shuffle);

    // A guest cannot enter the account panes, so `ui.start_pane` would strand
    // them on an empty one. Search is where they can actually do something.
    let start_pane = if guest {
        ytm_tui::app::Pane::Search
    } else {
        cfg.ui.start_pane.pane()
    };
    let mut state = AppState {
        volume,
        shuffle: cfg.playback.shuffle,
        guest,
        // Playlists by default rather than Home: your own playlists are what
        // most sessions start from, and Home costs a multi-page fetch before the
        // first useful frame. `ui.start_pane` changes it.
        pane: start_pane,
        sidebar_selected: ytm_tui::app::PANE_ORDER
            .iter()
            .position(|p| *p == start_pane)
            .unwrap_or(0),
        focus: if guest {
            // Straight into the query field: typing is the first useful thing a
            // guest can do.
            ytm_tui::app::Focus::SearchInput
        } else {
            ytm_tui::app::Focus::default()
        },
        ..Default::default()
    };
    // Skipped for a guest: the cache holds the previous session's library, and
    // those rows belong to panes a guest cannot open (FR-G9).
    if let Some(c) = cache.as_ref() {
        if !guest {
            app_loop::preload_from_cache(c, &mut state);
        } else if let Ok(v) = c.get_downloaded_tracks() {
            state.downloaded_tracks = v.into_iter().map(|dt| dt.to_track()).collect();
        }
    }
    if guest {
        state.push_toast(
            ytm_tui::app::ToastKind::Info,
            app_loop::GUEST_NOTICE,
            state.elapsed_ms,
        );
    }

    // The writer owns its connection: `rusqlite::Connection` is `Send` but not
    // `Sync`, so the loop cannot share one. A second reader handle is what WAL mode
    // is for, and a failure here costs only the preload, not the writes.
    let cache_reader = if cache.is_some() && !guest {
        match ytm_core::cache::Cache::open(&cache_path) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(error = %e, "no cache reader; playlists will load blank");
                None
            }
        }
    } else {
        None
    };
    // The startup reads are done; the connection moves onto its own thread so
    // the event loop never waits on a commit.
    let cache_writer = cache.map(app_loop::spawn_cache_writer);

    install_panic_hook();
    let mut guard = TerminalGuard::new(cfg.ui.mouse)?;

    // The cached frame goes up before anything touches the network (NFR-1): building
    // the source awaits a cookie round trip, which put a blank terminal on screen for
    // ~2.4s. The keymap is built from `[keys]` so a rebind applies on the first frame.
    let keymap = if cfg.keys.is_empty() {
        KeyMap::new(cfg.ui.vim_keys)
    } else {
        KeyMap::from_toml_str_with(&toml::to_string(&cfg.keys)?, cfg.ui.vim_keys)?
    };
    // The pre-probe frame cannot draw art: the picker does not exist yet.
    let mut art_probe = ytm_tui::widgets::art::ArtCache::disabled();
    guard
        .terminal
        .draw(|f| ytm_tui::render::render(f, &state, &theme, &keymap, &mut art_probe))?;
    tracing::info!(
        cached_playlists = state.playlists.len(),
        cached_tracks = state.tracks.len(),
        "first frame drawn"
    );

    // Probed after entering the alternate screen (what `from_query_stdio`'s docs
    // require) and after the first draw — it blocks up to 2s on a terminal that never
    // answers. Still before the event stream, or the reply reads as a key press.
    let art = if cfg.ui.album_art {
        ytm_tui::widgets::art::ArtCache::detect()
    } else {
        ytm_tui::widgets::art::ArtCache::disabled()
    };

    // Optional by design: no bus means no media keys and nothing else changes.
    let (media_tx, media_keys) = tokio::sync::mpsc::unbounded_channel();
    let media = mpris::attach(media_tx);

    // NOT awaited here — see `app_loop::run`'s `source_fut` parameter.
    let source_fut = build_source_or_guest(&cfg);
    let theme_file = cfg.ui.theme_file.as_deref().map(config::expand_tilde);
    let custom_theme = if theme_name == "custom" {
        Some(theme)
    } else if let Some(path) = &theme_file {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| ytm_tui::theme::Theme::from_toml_str(&t).ok())
    } else {
        None
    };
    let auto_reload_theme = cfg.ui.auto_reload_theme;
    let result = app_loop::run(
        &mut guard.terminal,
        state,
        source_fut,
        player,
        player_events,
        keymap,
        theme,
        cfg.ui.tick_ms,
        cfg.behaviour.clone(),
        // The empty-library hint is about an expired cookie, so it only applies
        // when there was a cookie to expire.
        cfg.auth.kind == config::AuthKind::Cookie && !guest,
        cache_reader,
        cache_writer,
        art,
        media,
        media_keys,
        cfg.config_path.clone(),
        theme_name,
        custom_theme,
        theme_file,
        auto_reload_theme,
        Some(audio_storage),
        cookie_path,
    )
    .await;

    // Drop the guard before returning, so an error report prints to a restored
    // terminal rather than into the alternate screen.
    drop(guard);

    // Best-effort: a cache dir we cannot write is not worth failing a clean exit
    // over, and the next run just starts from `playback.volume`.
    if let Ok(volume) = &result
        && let Err(e) = state_file::save(
            &state_path,
            &state_file::RuntimeState {
                volume: Some(*volume),
            },
        )
    {
        tracing::warn!(error = %e, "could not save the runtime state");
    }
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn shuffle_at_startup_is_sent_to_the_player_not_just_shown() {
        // playback.shuffle set the UI arrow but never reached the queue, so the
        // indicator said shuffle while playback stayed in order.
        use ytm_player::player::PlayerCommand;
        let (player, _rx) = ytm_player::mock::MockPlayer::new();
        apply_startup_shuffle(&player, true);
        assert!(
            matches!(
                player.commands().as_slice(),
                [PlayerCommand::SetShuffle(true)]
            ),
            "shuffle must reach the queue, got {:?}",
            player.commands()
        );

        let (off, _rx2) = ytm_player::mock::MockPlayer::new();
        apply_startup_shuffle(&off, false);
        assert!(
            off.commands().is_empty(),
            "shuffle off is the player's own default; sending it is noise"
        );
    }

    #[test]
    fn absent_cookie_starts_the_tui_in_guest_search() {
        let cfg = config::Config::default();
        assert!(guest_startup(&cfg));
    }

    #[test]
    fn configured_cookie_keeps_the_normal_startup_path() {
        // A file that really exists: the check is about reachability, not the
        // presence of a config key that may point at nothing.
        let dir = std::env::temp_dir().join("ytm-tui-guest-startup-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cookies.txt");
        std::fs::write(&path, "SAPISID=example").unwrap();
        let mut cfg = config::Config::default();
        cfg.auth.cookie_file = Some(path.clone());
        assert!(!guest_startup(&cfg));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_cookie_path_that_does_not_exist_still_starts_as_a_guest() {
        let mut cfg = config::Config::default();
        cfg.auth.cookie_file = Some("/nonexistent/ytm-tui/cookies.txt".into());
        assert!(guest_startup(&cfg));
    }

    #[test]
    fn no_arguments_launches_the_tui() {
        let c = Cli::parse_from(["ytm"]);
        assert!(c.command.is_none());
    }

    #[test]
    fn config_is_the_way_in() {
        // The subcommand that replaced `login`: cookie auth needs a config file,
        // not a sign-in flow.
        assert!(matches!(
            Cli::parse_from(["ytm", "config"]).command,
            Some(Command::Config { no_edit: false })
        ));
        assert!(matches!(
            Cli::parse_from(["ytm", "config", "--no-edit"]).command,
            Some(Command::Config { no_edit: true })
        ));
    }

    #[test]
    fn playlists_is_a_non_interactive_listing() {
        // Useful for scripting and for verifying auth without the TUI.
        assert!(matches!(
            Cli::parse_from(["ytm", "playlists"]).command,
            Some(Command::Playlists)
        ));
    }

    #[test]
    fn a_config_path_can_be_overridden() {
        let c = Cli::parse_from(["ytm", "--config", "/tmp/x.toml"]);
        assert_eq!(
            c.config.as_deref(),
            Some(std::path::Path::new("/tmp/x.toml"))
        );
    }

    #[test]
    fn an_unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["ytm", "frobnicate"]).is_err());
    }

    #[test]
    fn cache_clear_parses_as_a_nested_subcommand() {
        assert!(matches!(
            Cli::parse_from(["ytm", "cache", "clear"]).command,
            Some(Command::Cache {
                action: CacheAction::Clear
            })
        ));
    }

    #[test]
    fn cache_without_an_action_is_rejected() {
        // Better an error than silently doing nothing to someone's cache.
        assert!(Cli::try_parse_from(["ytm", "cache"]).is_err());
    }
}
