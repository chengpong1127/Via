import Foundation

/// The production planner boundary. It deliberately owns transport DTOs so the
/// presentation/domain model does not acquire API or provider-specific fields.
actor HTTPJourneyPlanner: JourneyPlanning {
    let endpoint: URL
    private let urlSession: URLSession

    init(baseURL: URL, urlSession: URLSession = .shared) {
        endpoint = baseURL.appendingPathComponent("v1/journey-plans")
        self.urlSession = urlSession
    }

    func plan(_ request: PlanningRequest) async throws -> JourneyResult {
        var urlRequest = URLRequest(url: endpoint)
        urlRequest.httpMethod = "POST"
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        urlRequest.setValue("application/json", forHTTPHeaderField: "Accept")
        urlRequest.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        do {
            urlRequest.httpBody = try JSONEncoder().encode(RequestPayload(request: request))
        } catch {
            throw JourneyPlanningError.unexpectedFailure
        }

        do {
            let (data, response) = try await urlSession.data(for: urlRequest)
            guard let httpResponse = response as? HTTPURLResponse else {
                throw JourneyPlanningError.unsupportedResponse
            }
            switch httpResponse.statusCode {
            case 200:
                return try ResponsePayload.decode(data).journeyResult()
            case 400:
                let error = try? ResponseError(data: data)
                throw JourneyPlanningError.invalidRequest(field: error?.field ?? "/")
            case 503:
                throw JourneyPlanningError.temporarilyUnavailable
            case 500:
                throw JourneyPlanningError.unexpectedFailure
            default:
                throw JourneyPlanningError.unexpectedFailure
            }
        } catch let error as JourneyPlanningError {
            throw error
        } catch is CancellationError {
            throw CancellationError()
        } catch let error as URLError {
            if error.code == .cancelled {
                throw CancellationError()
            }
            throw JourneyPlanningError.temporarilyUnavailable
        } catch is DecodingError {
            throw JourneyPlanningError.unsupportedResponse
        } catch {
            throw JourneyPlanningError.unexpectedFailure
        }
    }
}

enum ViaAPIConfiguration {
    static var baseURL: URL {
        if let configured = Bundle.main.object(forInfoDictionaryKey: "VIA_API_BASE_URL") as? String,
           let url = URL(string: configured),
           url.scheme != nil,
           url.host != nil {
#if !DEBUG
            precondition(url.scheme?.lowercased() == "https", "VIA_API_BASE_URL must use HTTPS in Release builds")
#endif
            return url
        }
#if DEBUG
        // Localhost is intentionally available only to development builds.
        return URL(string: "http://127.0.0.1:3000")!
#else
        preconditionFailure("VIA_API_BASE_URL must be configured for Release builds")
#endif
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

private struct RequestPayload: Encodable {
    let origin: PlacePayload
    let destination: PlacePayload
    let selectedModes: [String]
    let locale: String
    let departureSetting: DeparturePayload

    init(request: PlanningRequest) {
        origin = PlacePayload(place: request.origin)
        destination = PlacePayload(place: request.destination)
        selectedModes = TravelMode.selectable
            .filter(request.selectedModes.contains)
            .map(\.rawValue)
        locale = LocaleNormalization.bcp47(request.locale)
        departureSetting = DeparturePayload(setting: request.departureSetting)
    }

    enum CodingKeys: String, CodingKey {
        case origin
        case destination
        case selectedModes = "selected_modes"
        case locale
        case departureSetting = "departure_setting"
    }
}

private struct PlacePayload: Encodable {
    let coordinate: CoordinatePayload
    let displayText: String

    init(place: SelectedPlace) {
        coordinate = CoordinatePayload(coordinate: place.coordinate)
        displayText = place.displayText.precomposedStringWithCanonicalMapping
    }

    enum CodingKeys: String, CodingKey {
        case coordinate
        case displayText = "display_text"
    }
}

private struct CoordinatePayload: Encodable {
    let latitude: Double
    let longitude: Double

    init(coordinate: Coordinate) {
        latitude = coordinate.latitude
        longitude = coordinate.longitude
    }
}

private enum DeparturePayload: Encodable {
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

private struct ResponseError: Decodable {
    let field: String?

    init(data: Data) throws {
        let envelope = try JSONDecoder().decode(ErrorEnvelope.self, from: data)
        field = envelope.error.field
    }
}

private struct ErrorEnvelope: Decodable {
    let error: ErrorObject
}

private struct ErrorObject: Decodable {
    let field: String?
}

private struct ResponsePayload: Decodable {
    let apiVersion: String
    let semanticCatalogVersion: String
    let generatedAt: String
    let networkSnapshot: ServerNetworkSnapshot
    let journeyOptions: [ServerJourneyOption]?
    let empty: ServerEmpty?

    enum CodingKeys: String, CodingKey {
        case apiVersion = "api_version"
        case semanticCatalogVersion = "semantic_catalog_version"
        case generatedAt = "generated_at"
        case networkSnapshot = "network_snapshot"
        case journeyOptions = "journey_options"
        case empty
    }

    static func decode(_ data: Data) throws -> ResponsePayload {
        try JSONDecoder().decode(Self.self, from: data)
    }

    func journeyResult() throws -> JourneyResult {
        guard (journeyOptions != nil) != (empty != nil),
              let generatedAt = RFC3339.date(generatedAt),
              let snapshot = networkSnapshot.domainValue()
        else {
            throw JourneyPlanningError.unsupportedResponse
        }
        let options = try journeyOptions?.map { try $0.domainValue() } ?? []
        return JourneyResult(
            apiVersion: apiVersion,
            semanticCatalogVersion: semanticCatalogVersion,
            generatedAt: generatedAt,
            networkSnapshot: snapshot,
            options: options
        )
    }
}

private struct ServerEmpty: Decodable {
    let code: String
}

private struct ServerNetworkSnapshot: Decodable {
    let id: String
    let createdAt: String
    let liveCutoff: String
    let staticNetworkRevision: String
    let scheduleRevision: String

    enum CodingKeys: String, CodingKey {
        case id
        case createdAt = "created_at"
        case liveCutoff = "live_cutoff"
        case staticNetworkRevision = "static_network_revision"
        case scheduleRevision = "schedule_revision"
    }

    func domainValue() -> NetworkSnapshot? {
        guard let createdAt = RFC3339.date(createdAt),
              let liveCutoff = RFC3339.date(liveCutoff)
        else { return nil }
        return NetworkSnapshot(
            id: id,
            createdAt: createdAt,
            liveCutoff: liveCutoff,
            staticNetworkRevision: staticNetworkRevision,
            scheduleRevision: scheduleRevision
        )
    }
}

private struct ServerJourneyOption: Decodable {
    let id: String
    let matchingKey: String
    let journeyClass: String
    let label: String?
    let departureTime: String
    let arrivalTime: String
    let durationSeconds: Int
    let walkingSeconds: Int
    let transferCount: Int
    let travelModes: [String]
    let liveStatusSummary: ServerLiveStatusSummary
    let overviewGeometry: ServerGeometry
    let legs: [ServerLeg]

    enum CodingKeys: String, CodingKey {
        case id
        case matchingKey = "matching_key"
        case journeyClass = "class"
        case label
        case departureTime = "departure_time"
        case arrivalTime = "arrival_time"
        case durationSeconds = "duration_seconds"
        case walkingSeconds = "walking_seconds"
        case transferCount = "transfer_count"
        case travelModes = "travel_modes"
        case liveStatusSummary = "live_status_summary"
        case overviewGeometry = "overview_geometry"
        case legs
    }

    func domainValue() throws -> JourneyOption {
        guard let journeyClass = JourneyClass(rawValue: journeyClass),
              let departureTime = RFC3339.date(departureTime),
              let arrivalTime = RFC3339.date(arrivalTime),
              let liveStatusSummary = liveStatusSummary.domainValue(),
              let overview = overviewGeometry.coordinates()
        else { throw JourneyPlanningError.unsupportedResponse }
        let travelModes = try travelModes.map { rawValue -> TravelMode in
            guard let mode = TravelMode(rawValue: rawValue) else {
                throw JourneyPlanningError.unsupportedResponse
            }
            return mode
        }
        let legs = try legs.map { try $0.domainValue() }
        return JourneyOption(
            id: id,
            matchingKey: matchingKey,
            journeyClass: journeyClass,
            label: label.flatMap(JourneyLabel.init(rawValue:)),
            departureTime: departureTime,
            arrivalTime: arrivalTime,
            durationSeconds: durationSeconds,
            walkingSeconds: walkingSeconds,
            transferCount: transferCount,
            travelModes: travelModes,
            liveStatusSummary: liveStatusSummary,
            overview: overview,
            legs: legs
        )
    }
}

private struct ServerLiveStatusSummary: Decodable {
    let state: String
    let observedAt: String?

    enum CodingKeys: String, CodingKey {
        case state
        case observedAt = "observed_at"
    }

    func domainValue() -> LiveStatusSummary? {
        guard let state = LiveStatusState(rawValue: state) else { return nil }
        return LiveStatusSummary(state: state, observedAt: observedAt.flatMap(RFC3339.date))
    }
}

private struct ServerGeometry: Decodable {
    let encoding: String
    let points: String

    func coordinates() -> [Coordinate]? {
        guard encoding == "polyline6" else { return nil }
        return Polyline6.decode(points)
    }
}

private struct ServerPlace: Decodable {
    let displayName: String

    enum CodingKeys: String, CodingKey {
        case displayName = "display_name"
    }
}

private struct ServerLeg: Decodable {
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

    func domainValue() throws -> JourneyLeg {
        guard let mode = TravelMode(rawValue: mode) else {
            throw JourneyPlanningError.unsupportedResponse
        }
        let title: String
        switch mode {
        case .walking: title = "Walk"
        case .bus: title = details.serviceLabel ?? "Bus"
        case .metro: title = details.lineLabel ?? "Metro"
        case .youbike: title = "YouBike"
        }
        let subtitle = "\(from.displayName) → \(to.displayName)"
        let guidance = guidance.map { instruction in
            GuidanceInstruction(
                title: instruction.title,
                detail: instruction.parameterDetail
            )
        }
        return JourneyLeg(
            id: id,
            mode: mode,
            title: title,
            subtitle: subtitle,
            durationSeconds: durationSeconds,
            guidance: guidance
        )
    }
}

private struct ServerDetails: Decodable {
    let type: String
    let serviceLabel: String?
    let lineLabel: String?

    enum CodingKeys: String, CodingKey {
        case type
        case serviceLabel = "service_label"
        case lineLabel = "line_label"
    }
}

private struct ServerGuidance: Decodable {
    let code: String
    let parameters: [String: JSONValue]

    var title: String {
        code
            .split(separator: "_")
            .map { $0.capitalized }
            .joined(separator: " ")
    }

    var parameterDetail: String? {
        for key in ["service_label", "boarding_point_name", "pickup_station_name", "return_station_name"] {
            if case let .string(value)? = parameters[key] { return value }
        }
        return nil
    }
}

private enum JSONValue: Decodable {
    case string(String)
    case number(Double)
    case boolean(Bool)
    case object([String: JSONValue])
    case array([JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode(Bool.self) { self = .boolean(value) }
        else if let value = try? container.decode(Double.self) { self = .number(value) }
        else if let value = try? container.decode([String: JSONValue].self) { self = .object(value) }
        else { self = .array(try container.decode([JSONValue].self)) }
    }
}

private enum RFC3339 {
    static func string(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    static func date(_ value: String) -> Date? {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.date(from: value) ?? {
            formatter.formatOptions = [.withInternetDateTime]
            return formatter.date(from: value)
        }()
    }
}

private enum Polyline6 {
    static func decode(_ encoded: String) -> [Coordinate]? {
        var values: [Int] = []
        var current = 0
        var shift = 0
        for scalar in encoded.unicodeScalars {
            let value = Int(scalar.value) - 63
            guard value >= 0 else { return nil }
            current |= (value & 0x1f) << shift
            if value < 0x20 {
                let delta = (current & 1) == 1 ? ~(current >> 1) : current >> 1
                values.append(delta)
                current = 0
                shift = 0
            } else {
                shift += 5
                guard shift <= 30 else { return nil }
            }
        }
        guard values.count.isMultiple(of: 2), current == 0 else { return nil }
        var latitude = 0
        var longitude = 0
        return stride(from: 0, to: values.count, by: 2).compactMap { index in
            latitude += values[index]
            longitude += values[index + 1]
            return Coordinate(
                latitude: Double(latitude) / 1_000_000,
                longitude: Double(longitude) / 1_000_000
            )
        }
    }
}
