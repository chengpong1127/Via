import Foundation

enum JourneyMapper {
    static func result(from response: JourneyPlanResponseDTO) throws -> JourneyResult {
        guard (response.journeyOptions != nil) != (response.empty != nil),
              let generatedAt = RFC3339.date(response.generatedAt)
        else {
            throw JourneyPlanningError.unsupportedResponse
        }
        let revision = response.networkRevision.domainValue()
        let options = try response.journeyOptions?.map { try $0.domainValue() } ?? []
        return JourneyResult(
            apiVersion: response.apiVersion,
            semanticCatalogVersion: response.semanticCatalogVersion,
            generatedAt: generatedAt,
            networkRevision: revision,
            options: options
        )
    }

    static func invalidRequestField(from data: Data) -> String? {
        (try? JSONDecoder().decode(ErrorEnvelope.self, from: data))?.error.field
    }
}

private extension ServerNetworkRevision {
    func domainValue() -> NetworkRevision {
        NetworkRevision(
            staticNetworkRevision: staticNetworkRevision,
            scheduleRevision: scheduleRevision
        )
    }
}

private extension ServerJourneyOptionDTO {
    func domainValue() throws -> JourneyOption {
        guard let journeyClass = JourneyClass(rawValue: journeyClass),
              let departureTime = RFC3339.date(departureTime),
              let arrivalTime = RFC3339.date(arrivalTime),
              let liveStatusSummary = liveStatusSummary.domainValue(),
              let overview = overviewGeometry.coordinates()
        else {
            throw JourneyPlanningError.unsupportedResponse
        }
        let travelModes = try travelModes.map { rawValue -> JourneyMode in
            guard let mode = JourneyMode(rawValue: rawValue) else {
                throw JourneyPlanningError.unsupportedResponse
            }
            return mode
        }
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
            legs: try legs.map { try $0.domainValue() }
        )
    }
}

private extension ServerLiveStatusSummary {
    func domainValue() -> LiveStatusSummary? {
        guard let state = LiveStatusState(rawValue: state) else { return nil }
        return LiveStatusSummary(state: state, observedAt: observedAt.flatMap(RFC3339.date))
    }
}

private extension ServerGeometry {
    func coordinates() -> [Coordinate]? {
        guard encoding == "polyline6" else { return nil }
        return Polyline6.decode(points)
    }
}

private extension ServerLegDTO {
    func domainValue() throws -> JourneyLeg {
        guard let mode = JourneyMode(rawValue: mode) else {
            throw JourneyPlanningError.unsupportedResponse
        }
        let title: String
        let transit: TransitLegMetadata?
        switch mode {
        case .walking:
            guard details.type == "walking" else {
                throw JourneyPlanningError.unsupportedResponse
            }
            title = "Walk"
            transit = nil
        case .bikeShare:
            guard details.type == "bike_share" else {
                throw JourneyPlanningError.unsupportedResponse
            }
            title = "Bike share"
            transit = nil
        case .transit:
            guard details.type == "transit",
                  let transitMode = details.transitMode.flatMap(TransitMode.init(rawValue)),
                  let serviceID = details.serviceID,
                  let boardingPointID = details.boardingPointID,
                  let alightingPointID = details.alightingPointID
            else {
                throw JourneyPlanningError.unsupportedResponse
            }
            title = details.serviceLabel ?? transitMode.titleKey
            transit = TransitLegMetadata(
                mode: transitMode,
                serviceID: serviceID,
                serviceLabel: details.serviceLabel ?? transitMode.titleKey,
                headsign: details.headsign ?? "",
                boardingPointID: boardingPointID,
                alightingPointID: alightingPointID
            )
        }
        let subtitle = "\(from.displayName) → \(to.displayName)"
        let guidance = guidance.map { instruction in
            GuidanceInstruction(title: instruction.title, detail: instruction.parameterDetail)
        }
        return JourneyLeg(
            id: id,
            mode: mode,
            title: title,
            subtitle: subtitle,
            durationSeconds: durationSeconds,
            guidance: guidance,
            transit: transit
        )
    }
}

private extension ServerGuidance {
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

enum RFC3339 {
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
