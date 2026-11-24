use std::time::Duration;

use metrics_exporter_prometheus::{BuildError, PrometheusBuilder, PrometheusHandle};

pub fn install_prometheus() -> Result<PrometheusHandle, BuildError> {
    let handle = PrometheusBuilder::new()
        .set_buckets(&[
            0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
        ])?
        .install_recorder()?;

    let upkeep = handle.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            upkeep.run_upkeep();
        }
    });

    Ok(handle)
}
