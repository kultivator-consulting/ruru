//! Subscribes to `ruru-source` and prints what it sees.
//!
//! Run two of these against one source: both print frames, the source opens the device
//! once, and `status` reports two subscribers. That is the fan-out this whole component
//! exists to provide.

use anyhow::Result;
use clap::Parser;
use ruru_client::{DEFAULT_SOCKET_PATH, SourceClient};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Parser)]
#[command(name = "ruru-tap", version, about)]
struct Args {
    /// Source socket to subscribe to.
    #[arg(short, long, default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,

    /// Stop after this many frames. Runs until interrupted when unset.
    #[arg(short, long)]
    frames: Option<u64>,

    /// Label for this tap, so two of them are distinguishable in one terminal.
    #[arg(short, long, default_value = "tap")]
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = SourceClient::new(&args.socket);

    let mut sub = client.subscribe().await?;
    println!("[{}] subscribed to {}", args.name, args.socket.display());

    let start = Instant::now();
    let mut count = 0u64;
    let mut last_seq: Option<u64> = None;

    while let Some(frame) = sub.next().await? {
        count += 1;

        // Gaps are expected and healthy — a subscriber that falls behind is skipped to
        // the newest frame rather than stalling the capture loop.
        let gap = match last_seq {
            Some(previous) if frame.seq > previous + 1 => frame.seq - previous - 1,
            _ => 0,
        };
        last_seq = Some(frame.seq);

        let fps = count as f64 / start.elapsed().as_secs_f64();
        println!(
            "[{}] seq {:>8}  t {}  {}x{} {}  {:>7} bytes  {:5.1} fps{}",
            args.name,
            frame.seq,
            frame.timestamp_us,
            frame.width,
            frame.height,
            frame.fourcc,
            frame.data.len(),
            fps,
            if gap > 0 {
                format!("  (skipped {gap})")
            } else {
                String::new()
            },
        );

        if args.frames.is_some_and(|limit| count >= limit) {
            break;
        }
    }

    println!(
        "[{}] {} frames in {:.1}s",
        args.name,
        count,
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
