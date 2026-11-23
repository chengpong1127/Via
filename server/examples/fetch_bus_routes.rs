use std::{path::PathBuf, time::Duration};

use via_server::{
    config::{Environment, ViaConfig, load_dotenv, tdx_credentials},
    data::{
        bus_routes::{BUS_ROUTE_CITIES, BUS_ROUTE_FIELDS},
        fetch::sha256_hex,
    },
    tdx::TdxClient,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();
    let snapshot = PathBuf::from(std::env::args().nth(1).expect("snapshot dir"));
    let environment = Environment::from_env_or_development()?;
    let settings = ViaConfig::load_for_with(None, environment, |_| {})?;
    let base = settings.tdx.api_base_url.clone();
    let client = TdxClient::new(tdx_credentials()?, settings.tdx.clone())?;
    let manifest_path = snapshot.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
    for (city, slug) in BUS_ROUTE_CITIES {
        let name = format!("bus-routes-{slug}");
        let path = snapshot.join(format!("{name}.json"));
        if path.exists() {
            continue;
        }
        let mut body = None;
        for attempt in 0..4 {
            tokio::time::sleep(Duration::from_secs(61)).await;
            match client
                .get_bytes(
                    &format!("v2/Bus/Route/City/{city}"),
                    &[("$select", BUS_ROUTE_FIELDS), ("$format", "JSON")],
                )
                .await
            {
                Ok(bytes) => {
                    body = Some(bytes);
                    break;
                }
                Err(error) => eprintln!("{city} attempt {attempt}: {error}"),
            }
        }
        let body = body.ok_or(format!("{city} failed"))?;
        std::fs::write(&path, &body)?;
        manifest["sources"][&name] = serde_json::json!({
            "source_id": "tdx-bus-route-city",
            "source_url": format!("{base}v2/Bus/Route/City/{city}"),
            "sha256": sha256_hex(&body),
            "byte_size": body.len(),
        });
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
        eprintln!("{city} {} bytes", body.len());
    }
    Ok(())
}
