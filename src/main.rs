//! hermes-dms daemon entrypoint.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use hermes_dms::config::Config;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[derive(Parser)]
#[command(
    name = "hermes-dms",
    about = "Hermes desktop bridge daemon for DankMaterialShell"
)]
struct Cli {
    /// Path to the TOML config file (default: ~/.config/hermes-dms/config.toml).
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();

    let config_path = cli
        .config
        .or_else(Config::default_path)
        .unwrap_or_else(|| PathBuf::from("hermes-dms.toml"));

    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, path = %config_path.display(), "failed to load config");
            return ExitCode::FAILURE;
        }
    };

    info!(
        hermes_api_url = %config.hermes_api_url,
        mcp_listen_addr = %config.mcp_listen_addr,
        socket_path = %config.socket_path.display(),
        "hermes-dms starting"
    );

    let shutdown = CancellationToken::new();
    spawn_signal_handler(shutdown.clone());

    // Spawns the MCP HTTP server + health loop in the background and runs the
    // IPC server in the foreground, all observing `shutdown`.
    let exit = match hermes_dms::daemon::run(config, shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!(error = %e, "daemon exited with error");
            ExitCode::FAILURE
        }
    };
    info!("hermes-dms stopped");
    exit
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("hermes_dms=info,hermes-dms=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// Cancel `shutdown` on SIGTERM or SIGINT for graceful teardown.
fn spawn_signal_handler(shutdown: CancellationToken) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "failed to install SIGTERM handler");
                return;
            }
        };
        let mut int = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "failed to install SIGINT handler");
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => info!("received SIGTERM"),
            _ = int.recv() => info!("received SIGINT"),
        }
        shutdown.cancel();
    });
}
