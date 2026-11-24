use std::{io::Write, sync::Arc, time::Duration};

use via_server::{
    config::{Environment, ViaConfig, load_dotenv, tdx_credentials},
    live::{DirectLiveData, LiveDataSource},
    tdx::TdxClient,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_dotenv();
    let args: Vec<String> = std::env::args().collect();
    let stops: std::collections::BTreeMap<String, Vec<String>> =
        serde_json::from_str(&std::fs::read_to_string(&args[1])?)?;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args[2])?;
    let until = time::OffsetDateTime::parse(
        &args[3],
        &time::format_description::well_known::Rfc3339,
    )?;
    let environment = Environment::from_env_or_development()?;
    let settings = ViaConfig::load_for_with(None, environment, |_| {})?;
    let live = DirectLiveData::new(Arc::new(TdxClient::new(
        tdx_credentials()?,
        settings.tdx.clone(),
    )?));
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    while time::OffsetDateTime::now_utc() < until {
        tick.tick().await;
        for (scope, stop_uids) in &stops {
            let polled_at = time::OffsetDateTime::now_utc();
            let batch = live.bus_arrivals(scope, stop_uids).await;
            let record = serde_json::json!({
                "scope": scope,
                "polled_at": polled_at.unix_timestamp(),
                "records": batch.records,
                "unavailable": batch.unavailable.len(),
            });
            writeln!(out, "{record}")?;
            out.flush()?;
            eprintln!("{scope} {} rows", batch.records.len());
        }
    }
    Ok(())
}
