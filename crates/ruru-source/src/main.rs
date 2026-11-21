use anyhow::{Context, Result};
use clap::Parser;
use ruru_source::RunningSource;
use ruru_source::config::Config;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Owns the camera and fans frames out to every consumer over a Unix socket.
#[derive(Debug, Parser)]
#[command(name = "ruru-source", version, about)]
struct Args {
    /// Path to the TOML config.
    #[arg(short, long, default_value = "/etc/ruru/source.toml")]
    config: PathBuf,

    /// Override `source.socket_path`.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Hold the device open even with no subscribers (overrides `source.pin`).
    #[arg(long)]
    pin: bool,

    /// Start with capture refused until something enables it (overrides `source.enabled`).
    #[arg(long)]
    disabled: bool,

    /// Log filter, e.g. `info`, `debug`, `ruru_source=debug,info`.
    #[arg(long, env = "RURU_LOG", default_value = "info")]
    log: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&args.log).context("parsing --log filter")?)
        .init();

    let mut config = Config::load(&args.config)?;
    if let Some(socket) = args.socket {
        config.source.socket_path = socket;
    }
    if args.pin {
        config.source.pin = true;
    }
    if args.disabled {
        config.source.enabled = false;
    }
    config.validate()?;

    let source = RunningSource::start(config).await?;

    wait_for_signal().await;

    info!("shutting down");
    source.shutdown().await;
    Ok(())
}

async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen for SIGTERM; waiting on ctrl-c only");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("received SIGINT"),
        _ = term.recv() => info!("received SIGTERM"),
    }
}
