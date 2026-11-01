import Foundation

nonisolated struct PlanningRequestDTO: Encodable, Equatable, Sendable {
    let origin: SelectedPlaceDTO
    let destination: SelectedPlaceDTO
    let selectedModes: [String]
    let departureSetting: DepartureSettingDTO

    enum CodingKeys: String, CodingKey {
        case origin
        case destination
        case selectedModes = "selected_modes"
        case departureSetting = "departure_setting"
    }
}

nonisolated struct SelectedPlaceDTO: Encodable, Equatable, Sendable {
    let coordinate: CoordinateDTO
    let displayText: String

    enum CodingKeys: String, CodingKey {
        case coordinate
        case displayText = "display_text"
    }
}

nonisolated struct CoordinateDTO: Codable, Equatable, Sendable {
    let latitude: Double
    let longitude: Double
}

nonisolated struct DepartureSettingDTO: Encodable, Equatable, Sendable {
    let type: String
    let time: String?
}

nonisolated struct PlanningOutcomeDTO: Decodable, Sendable {
    let generatedAt: String
    let journeyOptions: [JourneyOptionDTO]?
    let empty: EmptyReasonDTO?

    enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at"
        case journeyOptions = "journey_options"
        case empty
    }
}

nonisolated struct EmptyReasonDTO: Decodable, Sendable {
    let code: String
}

nonisolated struct JourneyOptionDTO: Decodable, Sendable {
    let id: String
    let matchingKey: String
    let label: String?
    let departureTime: String
    let arrivalTime: String
    let durationSeconds: Int
    let walkingSeconds: Int
    let transferCount: Int
    let liveStatusSummary: LiveStatusSummaryDTO
    let overviewGeometry: GeometryDTO
    let legs: [LegDTO]

    enum CodingKeys: String, CodingKey {
        case id
        case matchingKey = "matching_key"
        case label
        case departureTime = "departure_time"
        case arrivalTime = "arrival_time"
        case durationSeconds = "duration_seconds"
        case walkingSeconds = "walking_seconds"
        case transferCount = "transfer_count"
        case liveStatusSummary = "live_status_summary"
        case overviewGeometry = "overview_geometry"
        case legs
    }
}

nonisolated struct LiveStatusSummaryDTO: Decodable, Sendable {
    let state: String
    let observedAt: String?

    enum CodingKeys: String, CodingKey {
        case state
        case observedAt = "observed_at"
    }
}

nonisolated struct GeometryDTO: Decodable, Sendable {
    let encoding: String
    let points: String
}

nonisolated struct PlaceDTO: Decodable, Sendable {
    let displayName: String
    let coordinate: CoordinateDTO
    let viaID: String?

    enum CodingKeys: String, CodingKey {
        case displayName = "display_name"
        case coordinate
        case viaID = "via_id"
    }
}

nonisolated struct LegDTO: Decodable, Sendable {
    let id: String
    let mode: String
    let from: PlaceDTO
    let to: PlaceDTO
    let departureTime: String
    let arrivalTime: String
    let durationSeconds: Int
    let geometry: GeometryDTO
    let guidance: [GuidanceDTO]
    let details: LegDetailsDTO
    let attribution: [AttributionDTO]

    enum CodingKeys: String, CodingKey {
        case id
        case mode
        case from
        case to
        case departureTime = "departure_time"
        case arrivalTime = "arrival_time"
        case durationSeconds = "duration_seconds"
        case geometry
        case guidance
        case details
        case attribution
    }
}

nonisolated struct LegDetailsDTO: Decodable, Sendable {
    let type: String
    let transitMode: String?
    let viaPatternID: String?
    let serviceLabel: String?
    let headsign: String?
    let pickupStationID: String?
    let pickupStationName: String?
    let dropoffStationID: String?
    let dropoffStationName: String?

    enum CodingKeys: String, CodingKey {
        case type
        case transitMode = "transit_mode"
        case viaPatternID = "via_pattern_id"
        case serviceLabel = "service_label"
        case headsign
        case pickupStationID = "pickup_station_id"
        case pickupStationName = "pickup_station_name"
        case dropoffStationID = "dropoff_station_id"
        case dropoffStationName = "dropoff_station_name"
    }
}

nonisolated struct GuidanceDTO: Decodable, Sendable {
    let code: String
    let parameters: [String: String]
}

nonisolated struct AttributionDTO: Decodable, Sendable {
    let text: String
    let url: String
    let licence: String
}
