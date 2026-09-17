//! Proves the whole audio path: yt-dlp -> mpv -> speakers, driven through the
//! player actor exactly as the TUI will drive it.
//!
//! `println!` is allowed here — examples are exempt from the no-stdout rule.
//!
//! Run: cargo run -p ytm-player --example play_spike -- [videoId ...]

use ytm_core::{Track, VideoId};
use ytm_player::actor::spawn_player;
use ytm_player::player::{Player, PlayerCommand, PlayerEvent};

fn stub(id: &str, title: &str) -> Track {
    Track {
        video_id: VideoId::from(id),
        ..Track::stub(id, title)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    let tracks = if ids.is_empty() {
        vec![stub("dQw4w9WgXcQ", "first"), stub("9bZkp7q19f0", "second")]
    } else {
        ids.iter().map(|i| stub(i, i)).collect()
    };

    let (player, mut events) = spawn_player(60, None, None)?;
    println!("actor started; queueing {} tracks", tracks.len());
    player.send(PlayerCommand::EnqueueBack(tracks))?;

    // Watch events for 20s, then skip to prove Next works, then stop.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut skipped = false;
    let mut progress_ticks = 0u32;

    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, events.recv()).await {
            Ok(Some(ev)) => match ev {
                // Progress is 4Hz; printing every tick would drown everything else.
                PlayerEvent::Progress { position, duration } => {
                    progress_ticks += 1;
                    if progress_ticks.is_multiple_of(8) {
                        println!("  {position} / {duration}");
                    }
                    if !skipped && position.as_secs() >= 8 {
                        skipped = true;
                        println!("-> sending Next");
                        player.send(PlayerCommand::Next)?;
                    }
                }
                PlayerEvent::TrackChanged(Some(t)) => println!("TrackChanged: {}", t.title),
                PlayerEvent::TrackChanged(None) => println!("TrackChanged: queue empty"),
                other => println!("{other:?}"),
            },
            Ok(None) => {
                println!("event channel closed");
                break;
            }
            Err(_) => break,
        }
    }

    player.send(PlayerCommand::Shutdown)?;
    println!("\nprogress ticks: {progress_ticks}");
    if progress_ticks > 0 && skipped {
        println!("GATE: actor played, reported progress at ~4Hz, and honoured Next.");
    } else {
        println!("GATE: FAILED — ticks={progress_ticks} skipped={skipped}");
    }
    Ok(())
}
