use std::collections::HashSet;

use serde::Deserialize;

use crate::{
    domain::{
        bikeshare::{BikeShareNetwork, BikeStation},
        location::GeoCoordinate,
    },
    tdx::bike_station_id,
};

use super::DataError;

#[derive(Deserialize)]
struct TdxStation {
    #[serde(rename = "StationUID")]
    station_uid: String,
    #[serde(rename = "StationName")]
    station_name: TdxStationName,
    #[serde(rename = "StationPosition")]
    station_position: TdxStationPosition,
    #[serde(rename = "ServiceType")]
    service_type: u8,
}

#[derive(Deserialize)]
struct TdxStationName {
    #[serde(rename = "Zh_tw")]
    traditional_chinese: Option<String>,
    #[serde(rename = "En")]
    english: Option<String>,
}

#[derive(Deserialize)]
struct TdxStationPosition {
    #[serde(rename = "PositionLat")]
    latitude: f64,
    #[serde(rename = "PositionLon")]
    longitude: f64,
}

pub fn parse_tdx_station_catalog<'a>(
    catalogs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<BikeShareNetwork, DataError> {
    let mut stations = Vec::new();
    let mut ids = HashSet::new();
    for (city, body) in catalogs {
        if !crate::tdx::BIKE_CITIES
            .iter()
            .any(|(scope, _)| *scope == city)
        {
            return Err(DataError::InvalidArtifactJson(format!(
                "unsupported TDX Bike city {city}"
            )));
        }
        let records = serde_json::from_str::<Vec<TdxStation>>(body)
            .map_err(|error| DataError::InvalidArtifactJson(error.to_string()))?;
        for record in records
            .into_iter()
            .filter(|record| record.service_type == 2)
        {
            let station_id = bike_station_id(city, record.station_uid.trim().to_owned());
            if station_id.as_str().ends_with(':') || !ids.insert(station_id.clone()) {
                return Err(DataError::InvalidArtifactJson(
                    "TDX bike station catalog contains an empty or duplicate StationUID".into(),
                ));
            }
            let name = record
                .station_name
                .traditional_chinese
                .filter(|name| !name.trim().is_empty())
                .or(record.station_name.english)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    DataError::InvalidArtifactJson(format!(
                        "TDX bike station {} has no display name",
                        station_id.as_str()
                    ))
                })?;
            let coordinate = GeoCoordinate {
                latitude: record.station_position.latitude,
                longitude: record.station_position.longitude,
            };
            if !coordinate.validate() {
                return Err(DataError::InvalidArtifactJson(format!(
                    "TDX bike station {} has invalid coordinates",
                    station_id.as_str()
                )));
            }
            stations.push(BikeStation::new(station_id, name, coordinate));
        }
    }
    Ok(BikeShareNetwork::new(stations).canonicalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_configured_cities_with_deterministic_city_scoped_ids() {
        let body = r#"[{"StationUID":"same", "StationName":{"Zh_tw":"Station"}, "StationPosition":{"PositionLat":24.0,"PositionLon":121.0},"ServiceType":2}]"#;
        let catalogs = [("Taipei", body), ("Taichung", body), ("Kaohsiung", body)];
        let first = parse_tdx_station_catalog(catalogs).unwrap();
        let second = parse_tdx_station_catalog(catalogs.into_iter().rev()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.stations.len(), 3);
        for (city, _) in catalogs {
            assert!(first.station(&bike_station_id(city, "same")).is_some());
        }
        assert!(parse_tdx_station_catalog([("Unknown", body)]).is_err());
    }

    #[test]
    fn maps_local_tdx_city_station_catalogs_to_availability_ids() {
        let network = parse_tdx_station_catalog([
            (
                "Taipei",
                include_str!("../../fixtures/tdx/raw/bike/taipei/station.json"),
            ),
            (
                "NewTaipei",
                include_str!("../../fixtures/tdx/raw/bike/new-taipei/station.json"),
            ),
        ])
        .unwrap();

        assert_eq!(network.stations.len(), 3);
        assert!(
            network
                .station(&bike_station_id("Taipei", "TPE-BIKE-001"))
                .is_some()
        );
        assert!(
            network
                .station(&bike_station_id("Taipei", "TPE-BIKE-002"))
                .is_some()
        );
        assert!(
            network
                .station(&bike_station_id("NewTaipei", "NWT-BIKE-001"))
                .is_some()
        );
        assert!(network.access_links.is_empty());
    }

    #[test]
    fn rejects_duplicate_tdx_station_uids_and_invalid_coordinates() {
        let taipei = r#"[{
            "StationUID":"same",
            "StationName":{"Zh_tw":"測試站"},
            "StationPosition":{"PositionLat":25.0,"PositionLon":121.5},
            "ServiceType":2
        }]"#;
        let duplicate_taipei = taipei.replace(
            "}]",
            "},{\"StationUID\":\"same\",\"StationName\":{\"Zh_tw\":\"另一站\"},\"StationPosition\":{\"PositionLat\":25.0,\"PositionLon\":121.5},\"ServiceType\":2}]",
        );

        assert!(parse_tdx_station_catalog([("Taipei", duplicate_taipei.as_str())]).is_err());

        let invalid = taipei.replace("25.0", "95.0");
        assert!(parse_tdx_station_catalog([("Taipei", invalid.as_str())]).is_err());
    }
}
