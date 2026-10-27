# ytm-tui

YouTube Music client for the terminal, written in Rust. Audio plays through `mpv`.

Browse Home, playlists, liked songs, albums and artists. Search tracks, manage the queue, and play music from the keyboard.

## Features

- Home recommendations, playlists, liked songs, albums, artists and online search.
- Download songs with `d`, or pre-cache the queue lookahead so the next track does not wait on a download.
- Vim and arrow navigation: `j`/`k` or arrows, `g`/`G`, Ctrl+d/u, visual range selection with `V`, and `1` through `8` to switch tabs.
- Queue controls: reorder tracks, play next, append with `a`, and clear.
- Album art in graphics-capable terminals such as Kitty, Sixel and iTerm2. Six themes: tokyonight, gruvbox, nord, dracula, dawn and paper.
- `ytm-tui` caches cover art on disk, up to 128 MiB by default with least-recently-used eviction. Repeat visits and later launches read from the cache instead of downloading the image again.
- Media keys and MPRIS desktop integration.
- Guest mode without an account, or sign in with cookies to reach your library.

## Installation

### Prerequisites

`ytm-tui` needs `mpv` for audio playback and `yt-dlp` for streaming and downloads.

- Arch, Manjaro, CachyOS and EndeavourOS:
    ```bash
    sudo pacman -S mpv yt-dlp
    ```
- Debian and Ubuntu:
    ```bash
    sudo apt update && sudo apt install mpv yt-dlp
    ```
- Fedora:
    ```bash
    sudo dnf install mpv yt-dlp
    ```
- macOS with Homebrew:
    ```bash
    brew install mpv yt-dlp
    ```
- Windows: run inside WSL2 with Ubuntu:
    ```bash
    sudo apt update && sudo apt install mpv yt-dlp
    ```

### 1. Install `ytm-tui`

#### Arch Linux: pacman package

Every release ships a pacman package, so pacman owns the files and can remove them again:

```bash
curl -fLO https://github.com/Pranab-kr/ytm-tui/releases/download/v0.3.0/ytm-tui-bin-0.3.0-1-x86_64.pkg.tar.zst
sudo pacman -U ytm-tui-bin-0.3.0-1-x86_64.pkg.tar.zst
```

Adjust the version for the release you want, or download the `.pkg.tar.zst` from the [Releases](https://github.com/Pranab-kr/ytm-tui/releases) page and run `sudo pacman -U ytm-tui-bin-*.pkg.tar.zst`. `mpv` and `yt-dlp` are installed automatically as dependencies.

The package is not on the AUR yet: `ytm-tui-bin` will be published there as soon as AUR account registration reopens. Until then, the command above is the same package.

#### Download a pre-built binary

For every other distribution, downloading a release is quicker than building from source.

1. Open the [Releases](https://github.com/Pranab-kr/ytm-tui/releases) page.
2. Download the archive for your platform, for example `ytm-tui-v0.3.0-x86_64-linux.tar.gz`.
3. Extract it and put `ytm-tui` on your PATH:
    ```bash
    tar -xzf ytm-tui-*.tar.gz
    cd ytm-tui-*/
    chmod +x ytm-tui
    mv ytm-tui ~/.local/bin/   # or: sudo mv ytm-tui /usr/local/bin/
    ```

#### Build from source

With Rust installed, run:

```bash
git clone https://github.com/Pranab-kr/ytm-tui.git
cd ytm-tui
cargo build --release
cp target/release/ytm-tui ~/.local/bin/
```

### 2. Run

Start the app:

```bash
ytm-tui
```

## Getting started

### 1. Guest mode

Launch `ytm-tui` without signing in to use guest mode. Search for tracks, albums and artists, stream them, and download songs with `d`. The library tabs, Home, Playlists and Fav, ask you to sign in.

### 2. Sign in

Export your YouTube Music session cookies once. They unlock your playlists, liked songs and recommendations.

1. Open a private window in Chrome, Firefox, Brave or Edge, and log in at [music.youtube.com](https://music.youtube.com).
2. Press `F12` and open the Network tab.
3. Hold Shift and click reload, or press Ctrl+Shift+R on Linux and Windows, or Cmd+Shift+R on macOS. A normal reload often comes from cache, so the `cookie:` header is missing. A clean reload includes the full request headers.
4. Click the first request to `music.youtube.com`. Under Request Headers, copy the entire value of the `cookie:` header. It starts with something like `VISITOR_INFO1_LIVE=...; SAPISID=...`.
5. Save that line to:
    - Linux: `~/.config/ytm-tui/cookies.txt`
    - macOS: `~/Library/Application Support/ytm-tui/cookies.txt`
6. Close the private window without logging out.
7. Run `ytm-tui config` to generate a commented example `config.toml` and open it in your editor. Pass `--no-edit` to write the file without opening it. Set:
    ```toml
    [auth]
    kind = "cookie"
    cookie_file = "~/.config/ytm-tui/cookies.txt"
    ```
8. Check the setup:
    ```bash
    ytm-tui playlists
    ```
    If the command lists your playlists, run `ytm-tui` and start listening.

### 3. Offline mode

With no connection the app still opens. It shows cached playlists and rows, loads covers from the disk cache, and plays downloaded songs. The pane heading reads `offline`, and a notice says that only the cache and downloads are available. Searches, uncached playlists and library refreshes need a connection. Start the app again after the connection returns.

## Keybindings

Press `?` inside the app to see the live list. Full table:

### Navigation

| Key                       | Action                                                                       |
| ------------------------- | ---------------------------------------------------------------------------- |
| `j` / `k` or down / up    | Move down or up                                                              |
| `h` / `l` or left / right | Go back or open the selected item                                            |
| `Enter`                   | Play a track or open a playlist or album                                     |
| `1` to `8`                | Switch tabs: Home, Playlists, Fav, Albums, Artists, Search, Queue, Downloads |
| `Tab`                     | Next tab                                                                     |
| `g` / `G`                 | Jump to the top or bottom                                                    |
| Ctrl+d / Ctrl+u           | Move half a page down or up                                                  |
| `zz`                      | Center the screen on the selected item                                       |
| `c`                       | Jump to the playing track                                                    |
| `q` / Ctrl+c              | Quit                                                                         |

### Playback

| Key       | Action                                  |
| --------- | --------------------------------------- |
| `Space`   | Play or pause                           |
| `n` / `p` | Next or previous track                  |
| `f` / `b` | Seek forward or backward                |
| `+` / `-` | Volume up or down                       |
| `m`       | Mute or unmute                          |
| `s`       | Toggle shuffle                          |
| `r`       | Cycle repeat between off, track and all |

### Queue and bulk selection

| Key       | Action                                                           |
| --------- | ---------------------------------------------------------------- |
| `a`       | Add the selected tracks or playlist to the queue                 |
| `e`       | Play next, after the current track                               |
| `d`       | Download the selected tracks                                     |
| `v`       | Mark or unmark one track                                         |
| `V`       | Start visual range selection, then press `j` or `k` to expand it |
| `Esc`     | Cancel visual selection or clear the filter                      |
| `A`       | Add marked tracks to a playlist                                  |
| `J` / `K` | Move a track down or up in the queue                             |
| `x`       | Remove a track from the queue or playlist                        |
| `C`       | Clear the queue                                                  |

### Search and filter

| Key | Action                                                     |
| --- | ---------------------------------------------------------- |
| `/` | Filter the current view locally, with no request           |
| `S` | Search YouTube Music online by title, artist or lyric line |

`S` sends the query to YouTube Music, so lyric lines work. `beneath the milky twilight` finds _Kiss Me_, `jinke sar ho ishq ki chhaon` finds _Chaiyya Chaiyya_, and `ami chini go chini tomare` finds the Rabindra Sangeet. The matching runs on YouTube's backend. `S` searches titles, artists and lyrics. The `/` filter is local and searches only the titles and artists already on screen.

### Appearance and settings

| Key | Action                      |
| --- | --------------------------- |
| `t` | Cycle color themes          |
| `,` | Open the configuration file |
| `?` | Show the help overlay       |

## Offline music and downloads

- Press `d` on a track, or on a visual selection, to download it.
- Open tab 8, Downloads, to see downloaded songs.
- When you play a downloaded song, `ytm-tui` plays the local file without contacting the network, even if you started it from a remote playlist or search.
- `ytm-tui cache clear` clears the metadata database and the cover-art cache. It leaves downloaded music and the audio cache in place.

## Configuration

Run `ytm-tui config` to create or edit the configuration file:

```bash
ytm-tui config
```

If the file does not exist, the command writes an example `config.toml` with every setting and keybinding commented out at its default, then opens the file in your editor, `$EDITOR`. Run `ytm-tui config --no-edit` to write the file without opening it.

The generated file documents every option: the startup tab, volume and seek steps, mouse support, vim keys, themes, colors and key rebinding.

Coming from `v0.1.x`? That release read `~/.config/ytm-cli`. Move it once with `mv ~/.config/ytm-cli ~/.config/ytm-tui`, otherwise the app starts signed out.

## Terms of service and privacy

`ytm-tui` uses YouTube Music's internal web API and `yt-dlp` for streaming. Use it for personal or educational purposes. The app sends no telemetry and collects no data. Tokens, cookies, cache and downloads stay on your machine.
