import Foundation

struct JourneyPlanRequestDTO: Encodable, Sendable {
    let origin: PlaceDTO
    let destination: PlaceDTO
    let selectedModes: [String]
    let locale: String
    let departureSetting: DepartureSettingDTO

    init(request: PlanningRequest) {
        origin = PlaceDTO(place: request.origin)
        destination = PlaceDTO(place: request.destination)
        selectedModes = TransportMode.selectable
            .filter(request.selectedModes.contains)
            .map(\.rawValue)
        locale = LocaleNormalization.bcp47(request.locale)
        departureSetting = DepartureSettingDTO(setting: request.departureSetting)
    }

    enum CodingKeys: String, CodingKey {
        case origin
        case destination
        case selectedModes = "selected_modes"
        case locale
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

enum LocaleNormalization {
    static func bcp47(_ identifier: String) -> String {
        let identifier = identifier.split(separator: "@", maxSplits: 1).first.map(String.init) ?? identifier
        let parts = identifier
            .replacingOccurrences(of: "_", with: "-")
            .split(separator: "-")
            .map(String.init)
        guard let language = parts.first,
              (2...8).contains(language.count),
              language.allSatisfy({ $0.isLetter })
        else { return "en" }

        var normalized = [language.lowercased()]
        for part in parts.dropFirst() {
            if part.count == 4, part.allSatisfy({ $0.isLetter }) {
                normalized.append(part.prefix(1).uppercased() + part.dropFirst().lowercased())
            } else if (part.count == 2 && part.allSatisfy({ $0.isLetter }))
                        || (part.count == 3 && part.allSatisfy({ $0.isNumber })) {
                normalized.append(part.uppercased())
            } else {
                normalized.append(part.lowercased())
            }
        }
        return normalized.joined(separator: "-")
    }
}
