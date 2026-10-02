use anyhow::Result;
use kurbis_hoster::control_plane::api;

fn main() -> Result<()> {
    let document = api::openapi_document();
    println!("{}", serde_json::to_string_pretty(&document)?);
    Ok(())
}
