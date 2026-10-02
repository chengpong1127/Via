import Foundation

struct ServerEmpty: Decodable, Sendable {
    let code: String
}

struct ErrorEnvelope: Decodable, Sendable {
    let error: ErrorObject
}

struct ErrorObject: Decodable, Sendable {
    let field: String?
}

struct ServerLiveStatusSummary: Decodable, Sendable {
    let state: String
    let observedAt: String?

    enum CodingKeys: String, CodingKey {
        case state
        case observedAt = "observed_at"
    }
}

struct ServerGeometry: Decodable, Sendable {
    let encoding: String
    let points: String
}

struct ServerPlace: Decodable, Sendable {
    let displayName: String

    enum CodingKeys: String, CodingKey {
        case displayName = "display_name"
    }
}

struct ServerDetails: Decodable, Sendable {
    let type: String
    let transitMode: String?
    let patternID: String?
    let boardingPointID: String?
    let alightingPointID: String?
    let serviceLabel: String?
    let headsign: String?
    let pickupStationID: String?
    let pickupStationName: String?
    let dropoffStationID: String?
    let dropoffStationName: String?

    enum CodingKeys: String, CodingKey {
        case type
        case transitMode = "transit_mode"
        case patternID = "via_pattern_id"
        case boardingPointID = "via_boarding_point_id"
        case alightingPointID = "via_alighting_point_id"
        case serviceLabel = "service_label"
        case headsign
        case pickupStationID = "pickup_station_id"
        case pickupStationName = "pickup_station_name"
        case dropoffStationID = "dropoff_station_id"
        case dropoffStationName = "dropoff_station_name"
    }
}

struct ServerGuidance: Decodable, Sendable {
    let code: String
    let parameters: [String: String]
}
