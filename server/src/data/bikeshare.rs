//! TDX static bike-share normalization.
//!
//! TDX field names and payload wrappers intentionally stop in this module. The
//! resulting domain network contains only stable IDs, display names, and
//! provider-neutral coordinates.

use std::{cmp::Ordering, path::Path};

use serde::Deserialize;

use crate::{
    domain::{
        bikeshare::{BikeShareNetwork, BikeStation},
        location::RequestCoordinate,
    },
    tdx::bike_station_id,
};

use super::{DataError, RawDocument, records};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BikeStationNormalizationDiagnostics {
    pub station_records: usize,
    pub malformed_records: usize,
    pub duplicate_records_removed: usize,
    pub stations_normalized: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct TdxBikeStationRecord {
    #[serde(rename = "StationUID")]
    station_uid: Option<String>,
    #[serde(rename = "StationName")]
    station_name: Option<TdxText>,
    #[serde(rename = "StationPosition")]
    station_position: Option<TdxPosition>,
    #[serde(rename = "PositionLat")]
    position_lat: Option<f64>,
    #[serde(rename = "PositionLon")]
    position_lon: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum TdxText {
    Localized(TdxLocalizedText),
    Plain(String),
}

#[derive(Clone, Debug, Deserialize)]
struct TdxLocalizedText {
    #[serde(rename = "Zh_tw")]
    zh_tw: Option<String>,
    #[serde(rename = "En")]
    en: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct TdxPosition {
    #[serde(rename = "PositionLat")]
    latitude: Option<f64>,
    #[serde(rename = "PositionLon")]
    longitude: Option<f64>,
}

impl TdxText {
    fn display_name(&self) -> Option<String> {
        let name = match self {
            Self::Localized(value) => value.zh_tw.as_deref().or(value.en.as_deref()),
            Self::Plain(value) => Some(value.as_str()),
        }?;
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }
}

impl TdxBikeStationRecord {
    fn normalize(self, city: &str) -> Option<BikeStation> {
        let station_uid = self.station_uid?.trim().to_owned();
        if station_uid.is_empty() {
            return None;
        }
        let name = self.station_name?.display_name()?;
        let position = self.station_position;
        let latitude = position
            .and_then(|position| position.latitude)
            .or(self.position_lat)?;
        let longitude = position
            .and_then(|position| position.longitude)
            .or(self.position_lon)?;
        let coordinate = RequestCoordinate {
            latitude,
            longitude,
        };
        coordinate
            .validate()
            .then(|| BikeStation::new(bike_station_id(city, station_uid), name, coordinate))
    }
}

pub(super) fn normalize_documents(
    documents: &[RawDocument],
) -> Result<(BikeShareNetwork, BikeStationNormalizationDiagnostics), DataError> {
    let mut stations = Vec::new();
    let mut diagnostics = BikeStationNormalizationDiagnostics::default();
    for document in documents
        .iter()
        .filter(|document| is_bike_station_document(&document.relative_path))
    {
        let Some(city) = bike_city(&document.relative_path) else {
            continue;
        };
        for record in records(&document.value) {
            diagnostics.station_records += 1;
            let station = serde_json::from_value::<TdxBikeStationRecord>(record.clone())
                .ok()
                .and_then(|record| record.normalize(city));
            let Some(station) = station else {
                diagnostics.malformed_records += 1;
                continue;
            };
            stations.push(station);
        }
    }

    stations.sort_by(compare_stations);
    let before_deduplication = stations.len();
    stations.dedup_by(|left, right| left.id == right.id);
    diagnostics.duplicate_records_removed = before_deduplication - stations.len();
    diagnostics.stations_normalized = stations.len();
    Ok((BikeShareNetwork::new(stations), diagnostics))
}

fn compare_stations(left: &BikeStation, right: &BikeStation) -> Ordering {
    left.id
        .as_str()
        .cmp(right.id.as_str())
        .then(left.name.cmp(&right.name))
        .then(
            left.coordinate
                .latitude
                .total_cmp(&right.coordinate.latitude),
        )
        .then(
            left.coordinate
                .longitude
                .total_cmp(&right.coordinate.longitude),
        )
}

fn is_bike_station_document(path: &Path) -> bool {
    let components = path.components().collect::<Vec<_>>();
    components.len() >= 3
        && components[components.len() - 3].as_os_str() == "bike"
        && components[components.len() - 1].as_os_str() == "station.json"
}

fn bike_city(path: &Path) -> Option<&str> {
    let components = path.components().collect::<Vec<_>>();
    let city = components
        .get(components.len().checked_sub(2)?)?
        .as_os_str()
        .to_str()?;
    match city {
        "taipei" => Some("Taipei"),
        "new-taipei" => Some("NewTaipei"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::normalize_documents;
    use crate::data::RawDocument;

    fn document(city: &str, value: serde_json::Value) -> RawDocument {
        RawDocument {
            relative_path: format!("bike/{city}/station.json").into(),
            bytes: serde_json::to_vec(&value).unwrap(),
            value,
            realtime_scope: None,
        }
    }

    fn station(uid: &str, name: &str, latitude: f64, longitude: f64) -> serde_json::Value {
        json!({
            "StationUID": uid,
            "StationName": {"Zh_tw": name, "En": name},
            "StationPosition": {"PositionLat": latitude, "PositionLon": longitude},
            "BikesCapacity": 20,
            "ServiceType": 2
        })
    }

    #[test]
    fn normalizes_tdx_station_schema_and_namespaces_city_identity() {
        let (network, diagnostics) = normalize_documents(&[
            document(
                "taipei",
                json!([station("LOCAL-1", "Taipei station", 25.0, 121.5)]),
            ),
            document(
                "new-taipei",
                json!([station("LOCAL-1", "New Taipei station", 25.01, 121.51)]),
            ),
        ])
        .unwrap();

        assert_eq!(diagnostics.stations_normalized, 2);
        assert_eq!(network.stations.len(), 2);
        assert_eq!(network.stations[0].id.as_str(), "tdx:NewTaipei:LOCAL-1");
        assert_eq!(network.stations[1].id.as_str(), "tdx:Taipei:LOCAL-1");
    }

    #[test]
    fn skips_malformed_records_and_chooses_duplicate_deterministically() {
        let first = document(
            "taipei",
            json!([
                station("DUP", "Zeta", 25.0, 121.5),
                station("DUP", "Alpha", 25.001, 121.501),
                {"StationUID": "missing-position", "StationName": {"Zh_tw": "bad"}},
                {"StationName": {"Zh_tw": "missing-id"}, "StationPosition": {"PositionLat": 25.0, "PositionLon": 121.5}}
            ]),
        );
        let second = document(
            "taipei",
            json!([
                station("DUP", "Alpha", 25.001, 121.501),
                station("DUP", "Zeta", 25.0, 121.5)
            ]),
        );
        let (first_network, first_diagnostics) = normalize_documents(&[first]).unwrap();
        let (second_network, _) = normalize_documents(&[second]).unwrap();

        assert_eq!(first_network, second_network);
        assert_eq!(first_network.stations[0].name, "Alpha");
        assert_eq!(first_diagnostics.malformed_records, 2);
        assert_eq!(first_diagnostics.duplicate_records_removed, 1);
    }
}
