# ytm-cli

A fast, keyboard-driven YouTube Music client for your terminal, written in Rust and powered by `mpv`.

Listen to music, search tracks, manage your playlists, download songs for offline listening, and control playback—all without leaving your terminal.

---

## Features

- **Everything in one place**: Browse Home recommendations, your Playlists, Liked Songs (Fav), Albums, Artists, and Search.
- **Offline Downloads & Caching**: Download tracks permanently with a single keypress (`d`), and enjoy automatic lookahead pre-caching for seamless, buffer-free playback.
- **Vim & Arrow Navigation**: Fast navigation with `j`/`k` or arrows, visual range selection (`V`), number keys (`1`–`8`) to switch tabs, and mouse scroll support.
- **Queue Control**: Reorder songs on the fly, play next (`e`), append to queue (`a`), or clear (`C`).
- **Album Art & Themes**: Full-color album art in graphics-capable terminals (Kitty, Sixel, iTerm2), and 6 built-in themes (`tokyonight`, `gruvbox`, `nord`, `dracula`, `dawn`, `paper`).
- **Media Keys & MPRIS**: Control playback using your keyboard's media keys or your desktop environment's music widget.
- **Guest Mode or Signed In**: Works instantly out of the box without an account, or sign in with your cookies to access your personal library.

---

## Installation

### Prerequisites

`ytm-cli` requires **`mpv`** (for audio playback) and **`yt-dlp`** (for streaming/downloading audio).

- **Arch / Manjaro / EndeavourOS**:
  ```bash
  sudo pacman -S mpv yt-dlp
  ```
- **Debian / Ubuntu**:
  ```bash
  sudo apt update && sudo apt install mpv yt-dlp
  ```
- **Fedora**:
  ```bash
  sudo dnf install mpv yt-dlp
  ```
- **macOS** (Homebrew):
  ```bash
  brew install mpv yt-dlp
  ```
- **Windows**:
  Run inside [WSL2 (Ubuntu)](https://learn.microsoft.com/en-us/windows/wsl/install):
  ```bash
  sudo apt update && sudo apt install mpv yt-dlp
  ```

---

### Step 1: Install `ytm-cli`

#### Option A: Download Pre-built Binary (Recommended)

1. Go to the [**Releases**](https://github.com/Pranab-kr/ytm-cli/releases) page.
2. Download the binary matching your operating system and CPU architecture (e.g., `ytm-cli-linux-x86_64.tar.gz`).
3. Extract the archive and place `ytm-cli` in your PATH:
   ```bash
   tar -xzf ytm-cli-*.tar.gz
   chmod +x ytm-cli
   mv ytm-cli ~/.local/bin/   # or sudo mv ytm-cli /usr/local/bin/
   ```

#### Option B: Build from Source (via Cargo)

If you have Rust installed:

```bash
git clone https://github.com/Pranab-kr/ytm-cli.git
cd ytm-cli
cargo build --release
cp target/release/ytm-cli ~/.local/bin/
```

---

### Step 2: Run

You can launch the app immediately:

```bash
ytm-cli
```

---

## Getting Started

### 1. Guest Mode (No Login Required)

If you launch `ytm-cli` without signing in, it runs in **Guest Mode**. You can immediately:
- Search for any song, album, or artist (`6` or `S`)
- Stream tracks and manage your current queue
- Download songs for offline listening (`d`)

Personal sections (Home, Playlists, Liked Songs) will prompt you to sign in.

---

### 2. Sign In (Access Your Personal Library)

To access your personal playlists, liked songs, and personalized recommendations, export your YouTube Music session cookies once:

1. Open a **Private / Incognito** window in your browser (Chrome, Firefox, Brave, Edge, etc.).
2. Go to [music.youtube.com](https://music.youtube.com) and log in to your Google account.
3. Press `F12` to open Developer Tools, then click the **Network** tab.
4. **Hold Shift and click the reload button** (or press `Ctrl+Shift+R` / `Cmd+Shift+R`). *Tip:* A normal reload is often served from browser cache and will not show the `cookie:` header; holding Shift forces a clean reload so the full request headers appear.
5. In the list of requests, click the first request to `music.youtube.com`. Under **Headers** → **Request Headers**, copy the entire value of the `cookie:` header (it starts with something like `VISITOR_INFO1_LIVE=...; SAPISID=...`).
6. Paste that single line into a text file at:
   - Linux: `~/.config/ytm-cli/cookies.txt`
   - macOS: `~/Library/Application Support/ytm-cli/cookies.txt`
7. Close the private browser window without logging out.
8. Configure `ytm-cli` to use your cookie file:
   ```bash
   ytm-cli config
   ```
   Set:
   ```toml
   [auth]
   kind = "cookie"
   cookie_file = "~/.config/ytm-cli/cookies.txt"
   ```
9. Verify your setup:
   ```bash
   ytm-cli playlists
   ```
   If it lists your playlists, you are all set! Run `ytm-cli` to start listening.

---

## Keybindings Cheat Sheet

Press `?` inside the app at any time to see the live keybindings list.

### Navigation
| Key | Action |
|---|---|
| `j` / `k` or `↓` / `↑` | Move down / up |
| `h` / `l` or `←` / `→` | Back / Open item |
| `Enter` | Play track or open playlist/album |
| `1` – `8` | Switch tabs (Home, Playlists, Fav, Albums, Artists, Search, Queue, Downloads) |
| `Tab` | Next tab |
| `g` / `G` | Jump to top / bottom |
| `Ctrl+d` / `Ctrl+u` | Half-page down / up |
| `zz` | Center screen on selected item |
| `c` | Jump to currently playing track |
| `q` / `Ctrl+c` | Quit |

### Playback
| Key | Action |
|---|---|
| `Space` | Play / Pause |
| `n` / `p` | Next / Previous track |
| `f` / `b` | Seek forward / backward |
| `+` / `-` | Volume up / down |
| `m` | Mute toggle |
| `s` | Shuffle toggle |
| `r` | Repeat mode (off / track / all) |

### Queue & Bulk Selection
| Key | Action |
|---|---|
| `a` | Add selected track(s) or playlist to queue |
| `e` | Play next (insert after current track) |
| `d` | Download selected track(s) for offline playback |
| `v` | Mark / unmark single track |
| `V` | Start visual range selection (use `j`/`k` to expand) |
| `Esc` | Cancel visual selection / clear filter |
| `A` | Add marked track(s) to a playlist |
| `J` / `K` | Move track down / up (inside Queue) |
| `x` | Remove track from queue or playlist |
| `C` | Clear entire queue |

### Search & Filter
| Key | Action |
|---|---|
| `/` | Filter the current view locally (instant) |
| `S` | Search YouTube Music online |

### Appearance & Settings
| Key | Action |
|---|---|
| `t` | Cycle color themes |
| `,` | Open configuration file |
| `?` | Show help overlay |

---

## Offline Music & Downloads

- Press `d` on any track (or highlighted visual selection) to download it.
- Switch to tab **8 (Downloads)** to view all downloaded songs.
- When you play a song that has been downloaded, `ytm-cli` automatically plays the local file with **zero network latency**, even if you start playback from a remote playlist or search.

---

## Configuration

Run `ytm-cli config` to edit your configuration. The generated file includes all available options with descriptions, commented out with their defaults:

- Change default startup tab (`playlists`, `home`, `songs`, etc.)
- Adjust volume steps and seek increments
- Enable or disable mouse support
- Toggle vim keys (`h`/`j`/`k`/`l`)
- Customize themes or colors
- Rebind any keyboard shortcut

---

## Terms of Service & Privacy

`ytm-cli` connects to YouTube Music's internal web API and uses `yt-dlp` for media streaming. This software is intended for personal and educational use. There is **no telemetry, no tracking, and no external data collection**—all tokens, cookies, cache files, and downloads stay strictly on your local machine.
