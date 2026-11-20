use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    BikeAvailabilityRecord, BusArrival, LiveBatch, LiveDataSource, MetroBoard, MetroPair, TraDelay,
    UnavailableReason,
};
use crate::tdx::{BIKE_CITIES, TdxClient, TdxClientError, odata_in_filter};

const BUS_ARRIVAL_PATH: &str = "v2/Bus/EstimatedTimeOfArrival/City";
const BIKE_AVAILABILITY_PATH: &str = "v2/Bike/Availability/City";
const METRO_LIVE_BOARD_PATH: &str = "v2/Rail/Metro/LiveBoard/TRTC";
const TRA_TRAIN_LIVE_BOARD_PATH: &str = "v3/Rail/TRA/TrainLiveBoard";

#[derive(Clone)]
pub struct DirectLiveData {
    tdx: Arc<TdxClient>,
    bus_arrival_path: Option<String>,
}

impl DirectLiveData {
    pub fn new(tdx: Arc<TdxClient>) -> Self {
        Self {
            tdx,
            bus_arrival_path: None,
        }
    }

    pub fn with_bus_arrival_path(mut self, path: impl Into<String>) -> Self {
        self.bus_arrival_path = Some(path.into());
        self
    }

    fn bus_path(&self, scope: &str) -> Option<String> {
        if let Some(path) = &self.bus_arrival_path {
            return Some(path.clone());
        }
        (!scope.is_empty()
            && scope
                .chars()
                .all(|character| character.is_ascii_alphanumeric()))
        .then(|| format!("{BUS_ARRIVAL_PATH}/{scope}"))
    }

    async fn bus_chunk(&self, path: &str, stops: &[String]) -> Result<Vec<BusArrival>, Failure> {
        let filter = odata_in_filter("StopUID", stops);
        let payload = self
            .tdx
            .get_json::<serde_json::Value>(path, &[("$filter", filter.as_str())])
            .await
            .map_err(Failure::from)?;
        let rows = payload
            .as_array()
            .or_else(|| payload.get("value").and_then(serde_json::Value::as_array))
            .ok_or(Failure::reason(UnavailableReason::InvalidResponse))?;
        Ok(rows
            .iter()
            .filter_map(|row| serde_json::from_value(row.clone()).ok())
            .collect())
    }

    async fn metro_chunk(&self, pairs: &[MetroPair]) -> Result<Vec<MetroBoard>, Failure> {
        let filter = metro_filter(pairs);
        let response_limit = self.tdx.settings().batch.metro_response_limit;
        let top = response_limit.to_string();
        let boards = self
            .tdx
            .get_json::<Vec<TdxMetroLiveBoard>>(
                METRO_LIVE_BOARD_PATH,
                &[
                    ("$filter", filter.as_str()),
                    ("$top", top.as_str()),
                    ("$format", "JSON"),
                ],
            )
            .await
            .map_err(Failure::from)?;
        if boards.len() >= response_limit
            || boards
                .iter()
                .any(|board| board.destination_station_id != board.destination_staion_id)
        {
            return Err(Failure::reason(UnavailableReason::InvalidResponse));
        }
        Ok(boards
            .into_iter()
            .map(|board| MetroBoard {
                line_id: board.line_id,
                station_id: board.station_id,
                destination_station_id: board.destination_station_id,
                service_status: board.service_status,
                estimate_minutes: board.estimate_minutes,
                source_update_time: board.source_update_time,
                update_time: board.update_time,
            })
            .collect())
    }
}

#[async_trait]
impl LiveDataSource for DirectLiveData {
    async fn bus_arrivals(&self, scope: &str, stop_uids: &[String]) -> LiveBatch<BusArrival> {
        let Some(path) = self.bus_path(scope) else {
            return LiveBatch::unavailable_all(stop_uids, UnavailableReason::InvalidConfiguration);
        };
        let mut batch = LiveBatch::default();
        for stops in stop_uids.chunks(self.tdx.settings().batch.bus_stops.max(1)) {
            if batch.is_rate_limited() {
                batch.mark_unavailable(stops, UnavailableReason::RateLimited);
                continue;
            }
            batch.upstream_requests += 1;
            match self.bus_chunk(&path, stops).await {
                Ok(records) => batch.records.extend(records),
                Err(failure) => failure.record(&mut batch, stops),
            }
        }
        batch
    }

    async fn bike_availability(
        &self,
        city: &str,
        station_uids: &[String],
    ) -> LiveBatch<BikeAvailabilityRecord> {
        if !BIKE_CITIES.iter().any(|(scope, _)| *scope == city) {
            return LiveBatch::unavailable_all(
                station_uids,
                UnavailableReason::InvalidConfiguration,
            );
        }
        let path = format!("{BIKE_AVAILABILITY_PATH}/{city}");
        let mut batch = LiveBatch::default();
        for stations in station_uids.chunks(self.tdx.settings().batch.bike_stations.max(1)) {
            let filter = odata_in_filter("StationUID", stations);
            let top = stations.len().to_string();
            batch.upstream_requests += 1;
            ::metrics::counter!("via_bike_availability_http_requests_total").increment(1);
            ::metrics::counter!("via_bike_availability_stations_fetched_total")
                .increment(stations.len() as u64);
            match self
                .tdx
                .get_json::<Vec<BikeAvailabilityRecord>>(
                    &path,
                    &[
                        ("$format", "JSON"),
                        ("$filter", filter.as_str()),
                        ("$top", top.as_str()),
                    ],
                )
                .await
            {
                Ok(records) => batch.records.extend(records.into_iter().filter(|record| {
                    record
                        .station_uid
                        .as_deref()
                        .is_none_or(|uid| stations.iter().any(|station| station == uid.trim()))
                })),
                Err(error) => Failure::from(error).record(&mut batch, stations),
            }
        }
        batch
    }

    async fn metro_boards(&self, pairs: &[MetroPair]) -> LiveBatch<MetroBoard, MetroPair> {
        let mut batch = LiveBatch::default();
        for chunk in pairs.chunks(self.tdx.settings().batch.metro_pairs.max(1)) {
            batch.upstream_requests += 1;
            match self.metro_chunk(chunk).await {
                Ok(records) => batch.records.extend(records),
                Err(failure) => failure.record(&mut batch, chunk),
            }
        }
        batch
    }

    async fn tra_delays(&self, train_numbers: &[String]) -> LiveBatch<TraDelay> {
        let mut batch = LiveBatch::default();
        if train_numbers.is_empty() {
            return batch;
        }
        let filter = train_filter(train_numbers);
        batch.upstream_requests += 1;
        match self
            .tdx
            .get_json::<TdxTraTrainLiveBoardResponse>(
                TRA_TRAIN_LIVE_BOARD_PATH,
                &[("$filter", filter.as_str()), ("$format", "JSON")],
            )
            .await
        {
            Ok(response) => {
                batch
                    .records
                    .extend(response.train_live_boards.into_iter().filter_map(|board| {
                        let train_no = board.train_no?.trim().to_owned();
                        (!train_no.is_empty()).then(|| TraDelay {
                            train_no,
                            delay_minutes: board.delay_minutes,
                            update_time: board.update_time,
                            board_update_time: response.update_time.clone(),
                            board_source_update_time: response.source_update_time.clone(),
                        })
                    }));
            }
            Err(error) => Failure::from(error).record(&mut batch, train_numbers),
        }
        batch
    }
}

struct Failure {
    reason: UnavailableReason,
    retry_after: Option<Duration>,
}

impl Failure {
    fn reason(reason: UnavailableReason) -> Self {
        Self {
            reason,
            retry_after: None,
        }
    }

    fn record<T, K: Clone>(self, batch: &mut LiveBatch<T, K>, ids: &[K]) {
        batch.mark_unavailable(ids, self.reason);
        if let Some(retry_after) = self.retry_after {
            batch.retry_after = Some(
                batch
                    .retry_after
                    .map_or(retry_after, |current| current.max(retry_after)),
            );
        }
    }
}

impl From<TdxClientError> for Failure {
    fn from(error: TdxClientError) -> Self {
        match error {
            TdxClientError::InvalidConfiguration => {
                Self::reason(UnavailableReason::InvalidConfiguration)
            }
            TdxClientError::RateLimited => Self::reason(UnavailableReason::RateLimited),
            TdxClientError::RateLimitedWithRetryAfter(retry_after) => Self {
                reason: UnavailableReason::RateLimited,
                retry_after: Some(retry_after),
            },
            TdxClientError::OAuthRequest(error) | TdxClientError::ApiRequest(error)
                if error.is_timeout() =>
            {
                Self::reason(UnavailableReason::TimedOut)
            }
            TdxClientError::OAuthRequest(_)
            | TdxClientError::InvalidOAuthResponse(_)
            | TdxClientError::ApiRequest(_)
            | TdxClientError::DownloadWrite(_)
            | TdxClientError::Credentials(_) => {
                Self::reason(UnavailableReason::UpstreamUnavailable)
            }
            TdxClientError::ResponseTooLarge | TdxClientError::InvalidJson(_) => {
                Self::reason(UnavailableReason::InvalidResponse)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct TdxMetroLiveBoard {
    #[serde(rename = "LineID")]
    line_id: String,
    #[serde(rename = "LineName")]
    _line_name: serde_json::Value,
    #[serde(rename = "StationID")]
    station_id: String,
    #[serde(rename = "StationName")]
    _station_name: serde_json::Value,
    #[serde(rename = "TripHeadSign")]
    _trip_head_sign: String,
    #[serde(rename = "DestinationStaionID")]
    destination_staion_id: String,
    #[serde(rename = "DestinationStationID")]
    destination_station_id: String,
    #[serde(rename = "DestinationStationName")]
    _destination_station_name: serde_json::Value,
    #[serde(rename = "ServiceStatus")]
    service_status: i32,
    #[serde(rename = "EstimateTime", default)]
    estimate_minutes: Option<i64>,
    #[serde(rename = "SrcUpdateTime")]
    source_update_time: String,
    #[serde(rename = "UpdateTime")]
    update_time: String,
}

#[derive(Debug, Deserialize)]
struct TdxTraTrainLiveBoardResponse {
    #[serde(rename = "UpdateTime")]
    update_time: String,
    #[serde(rename = "SrcUpdateTime")]
    source_update_time: String,
    #[serde(rename = "TrainLiveBoards")]
    train_live_boards: Vec<TdxTraTrainLiveBoard>,
}

#[derive(Debug, Deserialize)]
struct TdxTraTrainLiveBoard {
    #[serde(rename = "TrainNo")]
    train_no: Option<String>,
    #[serde(rename = "DelayTime")]
    delay_minutes: i64,
    #[serde(rename = "UpdateTime")]
    update_time: String,
}

fn metro_filter(pairs: &[MetroPair]) -> String {
    pairs
        .iter()
        .map(|pair| {
            format!(
                "(LineID eq '{}' and StationID eq '{}')",
                pair.line_id.replace('\'', "''"),
                pair.station_id.replace('\'', "''")
            )
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

fn train_filter(train_numbers: &[String]) -> String {
    train_numbers
        .iter()
        .map(|train_no| format!("TrainNo eq '{}'", train_no.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" or ")
}
