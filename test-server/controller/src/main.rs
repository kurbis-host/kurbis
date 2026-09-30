use std::{path::PathBuf, time::Duration};

use clap::Parser;
use vibe_controller::{controller::Controller, paths::RuntimePaths};

#[derive(Parser, Debug)]
#[command(version, about = "Codex backend for the Pumpkin Vibe test server")]
struct Args {
    #[arg(long, default_value = "../runtime")]
    runtime: PathBuf,

    #[arg(long, default_value = "codex")]
    codex: String,

    #[arg(long, default_value_t = 3600)]
    timeout_seconds: u64,

    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let controller = Controller::new(
        RuntimePaths::new(args.runtime),
        args.codex,
        Duration::from_secs(args.timeout_seconds),
    );

    if args.once {
        let processed = controller.run_once().await?;
        println!("{}", if processed { "processed" } else { "idle" });
        Ok(())
    } else {
        tokio::select! {
            result = controller.run() => result,
            signal = shutdown_signal() => {
                signal?;
                Ok(())
            }
        }
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
