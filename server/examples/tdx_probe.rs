use std::sync::Arc;

use via_server::{
    config::{ViaConfig, load_dotenv, tdx_credentials},
    tdx::TdxClient,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();
    let args: Vec<String> = std::env::args().collect();
    let mut settings = ViaConfig::load(None)?;
    settings.apply_environment()?;
    let client = Arc::new(TdxClient::new(
        tdx_credentials(&settings.tdx)?,
        settings.tdx.clone(),
    )?);
    let payload = client
        .get_json::<serde_json::Value>(
            &args[1],
            &args[2..]
                .chunks(2)
                .map(|pair| (pair[0].as_str(), pair[1].as_str()))
                .chain([("$format", "JSON")])
                .collect::<Vec<_>>(),
        )
        .await?;
    println!("{}", serde_json::to_string(&payload)?);
    Ok(())
}
