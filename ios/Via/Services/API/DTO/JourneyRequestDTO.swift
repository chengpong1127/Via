import Foundation

struct JourneyPlanRequestDTO: Encodable, Sendable {
    let origin: PlaceDTO
    let destination: PlaceDTO
    let selectedModes: [String]
    let departureSetting: DepartureSettingDTO

    init(request: PlanningRequest) {
        origin = PlaceDTO(place: request.origin)
        destination = PlaceDTO(place: request.destination)
        selectedModes = PlanningMode.selectable
            .filter(request.selectedModes.contains)
            .map(\.rawValue)
        departureSetting = DepartureSettingDTO(setting: request.departureSetting)
    }

    enum CodingKeys: String, CodingKey {
        case origin
        case destination
        case selectedModes = "selected_modes"
        case departureSetting = "departure_setting"
    }
}

struct PlaceDTO: Encodable, Sendable {
    let coordinate: CoordinateDTO
    let displayText: String

    init(place: Place) {
        coordinate = CoordinateDTO(coordinate: place.coordinate)
        displayText = place.displayText.precomposedStringWithCanonicalMapping
    }

    enum CodingKeys: String, CodingKey {
        case coordinate
        case displayText = "display_text"
    }
}

struct CoordinateDTO: Codable, Sendable {
    let latitude: Double
    let longitude: Double

    init(coordinate: Coordinate) {
        latitude = coordinate.latitude
        longitude = coordinate.longitude
    }
}

enum DepartureSettingDTO: Encodable, Sendable {
    case departNow
    case departAt(Date)
    case arriveBy(Date)

    init(setting: DepartureSetting) {
        switch setting {
        case .departNow: self = .departNow
        case let .departAt(date): self = .departAt(date)
        case let .arriveBy(date): self = .arriveBy(date)
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .departNow:
            try container.encode("depart_now", forKey: .type)
        case let .departAt(date):
            try container.encode("depart_at", forKey: .type)
            try container.encode(RFC3339.string(date), forKey: .time)
        case let .arriveBy(date):
            try container.encode("arrive_by", forKey: .type)
            try container.encode(RFC3339.string(date), forKey: .time)
        }
    }

    enum CodingKeys: String, CodingKey {
        case type
        case time
    }
}
