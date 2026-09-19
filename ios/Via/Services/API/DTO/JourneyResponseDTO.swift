import Foundation

struct JourneyPlanResponseDTO: Decodable, Sendable {
    let apiVersion: String
    let semanticCatalogVersion: String
    let generatedAt: String
    let networkRevision: ServerNetworkRevision
    let journeyOptions: [ServerJourneyOptionDTO]?
    let empty: ServerEmpty?

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case semanticCatalogVersion = "semantic_catalog_version"
        case generatedAt = "generated_at"
        case networkRevision = "network_revision"
        case journeyOptions = "journey_options"
        case empty
    }
}

struct ServerJourneyOptionDTO: Decodable, Sendable {
    let id: String
    let matchingKey: String
    let journeyClass: String
    let label: String?
    let departureTime: String
    let arrivalTime: String
    let durationSeconds: Int
    let walkingSeconds: Int
    let bicycleSeconds: Int?
    let bikeShareUses: Int?
    let transferCount: Int
    let travelModes: [String]
    let liveStatusSummary: ServerLiveStatusSummary
    let overviewGeometry: ServerGeometry
    let legs: [ServerLegDTO]

    enum CodingKeys: String, CodingKey {
        case id
        case matchingKey = "matching_key"
        case journeyClass = "class"
        case label
        case departureTime = "departure_time"
        case arrivalTime = "arrival_time"
        case durationSeconds = "duration_seconds"
        case walkingSeconds = "walking_seconds"
        case bicycleSeconds = "bicycle_seconds"
        case bikeShareUses = "bike_share_uses"
        case transferCount = "transfer_count"
        case travelModes = "travel_modes"
        case liveStatusSummary = "live_status_summary"
        case overviewGeometry = "overview_geometry"
        case legs
    }
}

struct ServerLegDTO: Decodable, Sendable {
    let id: String
    let mode: String
    let from: ServerPlace
    let to: ServerPlace
    let durationSeconds: Int
    let guidance: [ServerGuidance]
    let details: ServerDetails

    enum CodingKeys: String, CodingKey {
        case id
        case mode
        case from
        case to
        case durationSeconds = "duration_seconds"
        case guidance
        case details
    }
}
