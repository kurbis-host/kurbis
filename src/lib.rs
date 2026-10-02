pub mod config;
pub mod contracts;
pub mod control_plane;
pub mod db;
pub mod gateway;
pub mod model;
pub mod pumpkin_control;
pub mod routing;
pub mod snapshot;
pub mod worker;

pub fn init_tracing(default_filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
