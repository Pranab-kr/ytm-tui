# ytm-tui

YouTube Music client for the terminal, written in Rust, with playback through `mpv`.

Browse your Home feed, playlists, liked songs (Fav), albums and artists, search tracks, manage a queue, and play music, all without leaving the keyboard.

## Features

- Home recommendations, playlists, liked songs, albums, artists, and online search.
- Download songs to disk with `d`, or pre-cache the queue lookahead for smooth playback.
- Vim and arrow navigation: `j`/`k` or arrows, `g`/`G`, Ctrl+d/u, visual range selection (`V`), and `1`–`8` to switch tabs.
- Full queue control: reorder, play next, append with `a`, and clear.
- Album art in graphics-capable terminals (Kitty, Sixel, iTerm2) plus six themes: tokyonight, gruvbox, nord, dracula, dawn, paper.
- Cover art is cached on disk (up to 1 GiB, least-recently-used eviction), so repeat visits and later launches render previously fetched art without downloading it again.
- Media keys and MPRIS desktop integration.
- Guest mode with no account, or sign in with cookies to reach your personal library.

## Installation

### Prerequisites

`ytm-tui` needs `mpv` (audio playback) and `yt-dlp` (streaming and downloading).

- Arch / Manjaro / EndeavourOS:
  ```bash
  sudo pacman -S mpv yt-dlp
  ```
- Debian / Ubuntu:
  ```bash
  sudo apt update && sudo apt install mpv yt-dlp
  ```
- Fedora:
  ```bash
  sudo dnf install mpv yt-dlp
  ```
- macOS (Homebrew):
  ```bash
  brew install mpv yt-dlp
  ```
- Windows: run inside WSL2 (Ubuntu):
  ```bash
  sudo apt update && sudo apt install mpv yt-dlp
  ```

### Step 1: Install `ytm-tui`

#### Option A: Download a pre-built binary (recommended)

1. Open the [**Releases**](https://github.com/Pranab-kr/ytm-tui/releases) page.
2. Download the archive matching your platform, for example `ytm-tui-v0.2.2-x86_64-linux.tar.gz`.
3. Extract it and put `ytm-tui` on your PATH:
   ```bash
   tar -xzf ytm-tui-*.tar.gz
   cd ytm-tui-*/
   chmod +x ytm-tui
   mv ytm-tui ~/.local/bin/   # or: sudo mv ytm-tui /usr/local/bin/
   ```

#### Option B: Build from source with Cargo

With Rust installed:

```bash
git clone https://github.com/Pranab-kr/ytm-tui.git
cd ytm-tui
cargo build --release
cp target/release/ytm-tui ~/.local/bin/
```

### Step 2: Run

Launch the app:

```bash
ytm-tui
```

## Getting started

### 1. Guest mode (no login required)

Launch `ytm-tui` without signing in and it runs in guest mode. You can immediately search for tracks, albums and artists; stream them; and download songs with `d`. The personal library tabs (Home, Playlists, Liked Songs) prompt you to sign in.

### 2. Sign in (access your personal library)

Export your YouTube Music session cookies once to reach your playlists, liked songs and recommendations:

1. Open a private/incognito window (Chrome, Firefox, Brave, Edge, etc.) and log in to [music.youtube.com](https://music.youtube.com).
2. Press `F12`, then open the **Network** tab.
3. **Hold Shift and click the reload button** (or Ctrl+Shift+R / Cmd+Shift+R). A normal reload is often served from cache, so the `cookie:` header is missing; holding Shift forces a clean reload with full request headers.
4. Click the first request to `music.youtube.com`. Copy the entire value of the `cookie:` header under **Request Headers**. It starts with something like `VISITOR_INFO1_LIVE=...; SAPISID=...`.
5. Save that single line to:
   - Linux: `~/.config/ytm-tui/cookies.txt`
   - macOS: `~/Library/Application Support/ytm-tui/cookies.txt`
6. Close the private window without logging out.
7. Run `ytm-tui config` to generate an example `config.toml` (commented out with defaults) and open it in your editor. (Pass `--no-edit` to only generate the file.) Set
   ```toml
   [auth]
   kind = "cookie"
   cookie_file = "~/.config/ytm-tui/cookies.txt"
   ```
8. Check the setup:
   ```bash
   ytm-tui playlists
   ```
   If it lists your playlists, you are ready. Run `ytm-tui` to start listening.

## Keybindings

Press `?` inside the app for the live keybindings list. Full table:

### Navigation

| Key | Action |
|---|---|
| `j` / `k` or down / up | Move down / up |
| `h` / `l` or left / right | Back / open item |
| `Enter` | Play track or open playlist/album |
| `1`–`8` | Switch tabs (Home, Playlists, Fav, Albums, Artists, Search, Queue, Downloads) |
| `Tab` | Next tab |
| `g` / `G` | Jump to top / bottom |
| Ctrl+d / Ctrl+u | Half-page down / up |
| `zz` | Center screen on the selected item |
| `c` | Jump to the currently playing track |
| `q` / Ctrl+c | Quit |

### Playback

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `n` / `p` | Next / previous track |
| `f` / `b` | Seek forward / backward |
| `+` / `-` | Volume up / down |
| `m` | Mute toggle |
| `s` | Shuffle toggle |
| `r` | Repeat mode (off / track / all) |

### Queue and bulk selection

| Key | Action |
|---|---|
| `a` | Add selected track(s) or playlist to the queue |
| `e` | Play next (insert after the current track) |
| `d` | Download selected track(s) for offline playback |
| `v` | Mark / unmark a single track |
| `V` | Start visual range selection (`j`/`k` to expand) |
| `Esc` | Cancel visual selection / clear filter |
| `A` | Add marked track(s) to a playlist |
| `J` / `K` | Move a track down / up (inside the Queue) |
| `x` | Remove a track from the queue or playlist |
| `C` | Clear the entire queue |

### Search and filter

| Key | Action |
|---|---|
| `/` | Filter the current view locally (instant) |
| `S` | Search YouTube Music online: a title, an artist, **or a line of the lyrics** |

`S` sends the query straight to YouTube Music, so lyric lines work:
`beneath the milky twilight` finds *Kiss Me*, `jinke sar ho ishq ki chhaon` finds
*Chaiyya Chaiyya*, and `ami chini go chini tomare` finds the Rabindra Sangeet. The
matching happens in their backend, not here. `S` filters titles, artists and lyrics;
the `/` filter is local and only sees titles and artists already on screen.

### Appearance and settings

| Key | Action |
|---|---|
| `t` | Cycle color themes |
| `,` | Open the configuration file |
| `?` | Show the help overlay |

## Offline music and downloads

- Press `d` on a track (or a highlighted visual selection) to download it.
- Switch to tab **8 (Downloads)** to see all downloaded songs.
- When you play a downloaded song, `ytm-tui` plays the local file, with zero network latency, even if you start it from a remote playlist or search.
- `ytm-tui cache clear` clears the metadata database and the cover-art cache. Downloaded music and the audio cache are left alone.

## Configuration

Run `ytm-tui config` to create or edit the configuration file:

```bash
ytm-tui config
```

If the file does not exist, the command writes an example `config.toml` with every setting and keybinding commented out at its default, then opens it in your editor (`$EDITOR`). Run `ytm-tui config --no-edit` to only generate the file.

The generated file documents every option: the startup tab, volume and seek steps, mouse support, vim keys, themes, colors, and any key rebinding.

## Terms of service and privacy

`ytm-tui` uses YouTube Music's internal web API and `yt-dlp` for streaming. It is for personal and educational use. There is no telemetry, no tracking, and no data collection. Tokens, cookies, cache and downloads stay on your local machine.