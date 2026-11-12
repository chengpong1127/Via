import Foundation

nonisolated enum PlanningMapper {
    static func dto(for request: PlanningRequest) -> PlanningRequestDTO {
        PlanningRequestDTO(
            origin: selectedPlace(request.origin),
            destination: selectedPlace(request.destination),
            selectedModes: PlanningMode.allCases.filter(request.modes.contains).map(\.rawValue),
            departureSetting: DepartureSettingDTO(type: "depart_now")
        )
    }

    static func plan(from dto: PlanningOutcomeDTO) throws -> JourneyPlan {
        guard let generatedAt = RFC3339.date(from: dto.generatedAt) else {
            throw JourneyPlanningError.unsupportedResponse
        }
        switch (dto.journeyOptions, dto.empty) {
        case let (options?, nil):
            return JourneyPlan(generatedAt: generatedAt, options: try options.map(option))
        case (nil, _?):
            return JourneyPlan(generatedAt: generatedAt, options: [])
        default:
            throw JourneyPlanningError.unsupportedResponse
        }
    }

    static func displayText(_ text: String) -> String {
        let normalized = text
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .precomposedStringWithCanonicalMapping
        let clipped = String(normalized.prefix(200))
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return clipped.isEmpty ? "Pinned location" : clipped
    }

    private static func selectedPlace(_ place: Place) -> SelectedPlaceDTO {
        SelectedPlaceDTO(
            coordinate: CoordinateDTO(latitude: place.coordinate.latitude, longitude: place.coordinate.longitude),
            displayText: displayText(place.name)
        )
    }

    private static func option(_ dto: JourneyOptionDTO) throws -> JourneyOption {
        guard let departure = RFC3339.date(from: dto.departureTime),
              let arrival = RFC3339.date(from: dto.arrivalTime),
              let liveStatus = LiveStatus(rawValue: dto.liveStatusSummary.state),
              let overview = coordinates(dto.overviewGeometry)
        else {
            throw JourneyPlanningError.unsupportedResponse
        }
        return JourneyOption(
            id: dto.id,
            matchingKey: dto.matchingKey,
            departureTime: departure,
            arrivalTime: arrival,
            duration: TimeInterval(dto.durationSeconds),
            walkingDuration: TimeInterval(dto.walkingSeconds),
            transferCount: dto.transferCount,
            liveStatus: liveStatus,
            liveObservedAt: dto.liveStatusSummary.observedAt.flatMap(RFC3339.date(from:)),
            overview: overview,
            legs: try dto.legs.map(leg)
        )
    }

    private static func leg(_ dto: LegDTO) throws -> Leg {
        guard let departure = RFC3339.date(from: dto.departureTime),
              let arrival = RFC3339.date(from: dto.arrivalTime),
              let path = coordinates(dto.geometry)
        else {
            throw JourneyPlanningError.unsupportedResponse
        }
        return Leg(
            id: dto.id,
            kind: try kind(mode: dto.mode, details: dto.details),
            from: stop(dto.from),
            to: stop(dto.to),
            departureTime: departure,
            arrivalTime: arrival,
            duration: TimeInterval(dto.durationSeconds),
            path: path,
            guidance: dto.guidance.map { Guidance(code: $0.code, parameters: $0.parameters) },
            attributions: dto.attribution.map {
                Attribution(text: $0.text, url: URL(string: $0.url), licence: $0.licence)
            }
        )
    }

    private static func kind(mode: String, details: LegDetailsDTO) throws -> LegKind {
        switch (mode, details.type) {
        case ("walking", "walking"):
            return .walking
        case ("transit", "transit"):
            guard let mode = details.transitMode.flatMap(TransitMode.init(rawValue:)),
                  let patternID = details.viaPatternID,
                  let serviceLabel = details.serviceLabel,
                  let headsign = details.headsign
            else {
                throw JourneyPlanningError.unsupportedResponse
            }
            return .transit(TransitRide(
                mode: mode,
                serviceLabel: serviceLabel,
                headsign: headsign,
                patternID: patternID,
                intermediateStops: details.intermediateStops?.map(stop) ?? []
            ))
        case ("bicycle", "bike_share"):
            guard let pickupID = details.pickupStationID,
                  let pickupName = details.pickupStationName,
                  let dropoffID = details.dropoffStationID,
                  let dropoffName = details.dropoffStationName
            else {
                throw JourneyPlanningError.unsupportedResponse
            }
            return .bikeShare(BikeShareRide(
                pickupStationID: pickupID,
                pickupStationName: pickupName,
                dropoffStationID: dropoffID,
                dropoffStationName: dropoffName
            ))
        default:
            throw JourneyPlanningError.unsupportedResponse
        }
    }

    private static func stop(_ dto: PlaceDTO) -> Stop {
        Stop(
            name: dto.displayName,
            coordinate: Coordinate(latitude: dto.coordinate.latitude, longitude: dto.coordinate.longitude),
            viaID: dto.viaID,
            arrivalTime: dto.arrivalTime.flatMap(RFC3339.date(from:))
        )
    }

    private static func coordinates(_ geometry: GeometryDTO) -> [Coordinate]? {
        guard geometry.encoding == "polyline6" else { return nil }
        return Polyline6.decode(geometry.points)
    }
}

nonisolated enum RFC3339 {
    static func date(from string: String) -> Date? {
        if let date = try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(string) {
            return date
        }
        return try? Date.ISO8601FormatStyle().parse(string)
    }
}

nonisolated enum Polyline6 {
    static func decode(_ encoded: String) -> [Coordinate]? {
        var coordinates: [Coordinate] = []
        var latitude = 0
        var longitude = 0
        var iterator = encoded.unicodeScalars.makeIterator()

        func next() -> Int?? {
            var result = 0
            var shift = 0
            while true {
                guard let scalar = iterator.next() else {
                    return shift == 0 ? .some(nil) : nil
                }
                let value = Int(scalar.value) - 63
                guard (0..<64).contains(value), shift <= 30 else { return nil }
                result |= (value & 0x1f) << shift
                shift += 5
                if value < 0x20 { break }
            }
            return .some((result & 1) == 1 ? ~(result >> 1) : result >> 1)
        }

        while true {
            guard let latitudeDelta = next() else { return nil }
            guard let latitudeDelta else { return coordinates }
            guard let longitudeDelta = next(), let longitudeDelta else { return nil }
            latitude += latitudeDelta
            longitude += longitudeDelta
            coordinates.append(Coordinate(
                latitude: Double(latitude) / 1_000_000,
                longitude: Double(longitude) / 1_000_000
            ))
        }
    }
}
