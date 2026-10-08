pub mod ci;
pub mod config;
pub mod contracts;
pub mod control_plane;
pub mod db;
pub mod gateway;
pub mod model;
pub mod pumpkin_control;
pub mod routing;
pub mod runtime_api_proxy;
pub mod runtime_stats;
pub mod snapshot;
pub mod worker;
pub mod workload_ingress;

/// Full source revision embedded into every Rust application binary by the release build.
pub const BUILD_REVISION: &str = match option_env!("KURBIS_BUILD_REVISION") {
    Some(revision) => revision,
    None => "development",
};

pub fn init_tracing(default_filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
    tracing::info!(build_revision = BUILD_REVISION, "binary build revision");
}
