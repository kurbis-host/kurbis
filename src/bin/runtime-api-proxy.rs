use std::io::{Read, Write};

use anyhow::Result;

#[cfg(debug_assertions)]
use anyhow::{Context, ensure};
#[cfg(debug_assertions)]
use serde::Deserialize;
#[cfg(debug_assertions)]
use serde_json::json;

#[tokio::main]
async fn main() -> Result<()> {
    kurbis_hoster::init_tracing("kurbis_hoster=info");
    match std::env::args().nth(1).as_deref() {
        Some("--health-check") => kurbis_hoster::runtime_api_proxy::health_check().await,
        Some("--verify-upstream") => {
            kurbis_hoster::runtime_api_proxy::verify_configured_upstream().await
        }
        #[cfg(debug_assertions)]
        Some("--probe-http") => probe_http().await,
        #[cfg(debug_assertions)]
        Some("--probe-egress") => probe_egress().await,
        Some("--hash-stdin") => hash_stdin(),
        Some(_) => anyhow::bail!("unsupported runtime API proxy command"),
        None => kurbis_hoster::runtime_api_proxy::run_from_env().await,
    }
}

#[cfg(debug_assertions)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeHttpRequest {
    url: String,
    method: String,
    bearer_token: String,
    body: Option<serde_json::Value>,
}

#[cfg(debug_assertions)]
async fn probe_http() -> Result<()> {
    use reqwest::{Client, Method, redirect::Policy};
    use std::time::Duration;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let probe: ProbeHttpRequest = serde_json::from_str(&input)?;
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let method = Method::from_bytes(probe.method.as_bytes())?;
    let mut request = client
        .request(method, probe.url)
        .bearer_auth(probe.bearer_token);
    if let Some(body) = probe.body {
        request = request.json(&body);
    }
    let response = request.send().await.context("probe HTTP request failed")?;
    let status = response.status().as_u16();
    let body = response.text().await?;
    ensure!(
        body.len() <= 2 * 1024 * 1024,
        "probe response exceeded size limit"
    );
    println!("{}", json!({"status": status, "body": body}));
    Ok(())
}

#[cfg(debug_assertions)]
async fn probe_egress() -> Result<()> {
    use reqwest::{Client, redirect::Policy};
    use std::time::Duration;

    let mut target = String::new();
    std::io::stdin().read_to_string(&mut target)?;
    let client = Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?;
    match client.get(target.trim()).send().await {
        Ok(response) => println!(
            "{}",
            json!({"reachable": true, "status": response.status().as_u16()})
        ),
        Err(_) => println!("{}", json!({"reachable": false})),
    }
    Ok(())
}

fn hash_stdin() -> Result<()> {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    std::io::stdout().write_all(blake3::hash(&bytes).to_hex().as_bytes())?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}
