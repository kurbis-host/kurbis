use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

use clap::Parser;
use kurbis_ai_controller::{controller::Controller, paths::RuntimePaths};

#[derive(Parser, Debug)]
#[command(version, about = "Codex backend for the Kurbis AI test server")]
struct Args {
    #[arg(long, default_value = "../runtime")]
    runtime: PathBuf,

    #[arg(long, default_value = "codex")]
    codex: String,

    #[arg(long, default_value_t = 3600)]
    timeout_seconds: u64,

    #[arg(long)]
    once: bool,

    #[arg(long, requires = "job_id")]
    mcp_server: bool,

    #[arg(long)]
    job_id: Option<Uuid>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let paths = RuntimePaths::new(args.runtime);
    if args.mcp_server {
        return kurbis_ai_controller::mcp::serve_stdio(
            paths,
            args.job_id
                .expect("clap requires --job-id for --mcp-server"),
        )
        .await;
    }
    anyhow::ensure!(
        args.job_id.is_none(),
        "--job-id is only valid with --mcp-server"
    );
    let controller = Controller::new(paths, args.codex, Duration::from_secs(args.timeout_seconds));

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
