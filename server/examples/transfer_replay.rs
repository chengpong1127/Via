// Question (EXP-009): re-run the walking-transfer stage on an existing artifact with
// place-aware fan-out retention. Usage: VIA_PLACE_FANOUT=0|1 transfer_replay <artifact> <edges.json>
use std::{path::PathBuf, sync::Arc};

use via_server::{
    config::ViaConfig,
    data::{load_static_artifact, static_transfers},
    providers::ValhallaProvider,
    street::DependencyHealth,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let settings = ViaConfig::load(None)?;
    let mut artifact = load_static_artifact(&PathBuf::from(&args[1]))?;
    static_transfers::PLACE_FANOUT.store(
        std::env::var("VIA_PLACE_FANOUT").is_ok_and(|v| v == "1"),
        std::sync::atomic::Ordering::SeqCst,
    );
    let street = Arc::new(ValhallaProvider::new(settings.valhalla.provider_config(true)?)?);
    assert!(street.healthy().await, "valhalla unhealthy");
    let before = artifact.transit.transfer_edges.len();
    let stats = static_transfers::enrich_network_with_static_transfers(
        &mut artifact.transit,
        street.as_ref(),
        settings.preprocessing.transfers,
    )
    .await?;
    println!("edges before={before} after={}", artifact.transit.transfer_edges.len());
    println!("{stats:?}");
    std::fs::write(&args[2], serde_json::to_vec(&artifact.transit.transfer_edges)?)?;
    Ok(())
}
