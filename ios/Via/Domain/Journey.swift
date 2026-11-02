import Foundation

nonisolated struct JourneyPlan: Sendable {
    let generatedAt: Date
    let options: [JourneyOption]
}

nonisolated enum JourneyLabel: String, Sendable {
    case bestNow = "best_now"
    case fewerTransfers = "fewer_transfers"
    case lessWalking = "less_walking"
}

nonisolated enum TransitMode: String, Sendable {
    case bus
    case metro
    case rail
    case highSpeedRail = "high_speed_rail"
}

nonisolated enum LiveStatus: String, Sendable {
    case fresh
    case degraded
    case unavailable
    case notApplicable = "not_applicable"
}

nonisolated struct JourneyOption: Identifiable, Sendable {
    let id: String
    let matchingKey: String
    let label: JourneyLabel?
    let departureTime: Date
    let arrivalTime: Date
    let duration: TimeInterval
    let walkingDuration: TimeInterval
    let transferCount: Int
    let liveStatus: LiveStatus
    let liveObservedAt: Date?
    let overview: [Coordinate]
    let legs: [Leg]

    var attributions: [Attribution] {
        var seen = Set<Attribution>()
        return legs.flatMap(\.attributions).filter { seen.insert($0).inserted }
    }
}

nonisolated struct Stop: Hashable, Sendable {
    let name: String
    let coordinate: Coordinate
    let viaID: String?
}

nonisolated struct TransitRide: Hashable, Sendable {
    let mode: TransitMode
    let serviceLabel: String
    let headsign: String
    let patternID: String
    var intermediateStops: [Stop] = []
}

nonisolated struct BikeShareRide: Hashable, Sendable {
    let pickupStationID: String
    let pickupStationName: String
    let dropoffStationID: String
    let dropoffStationName: String
}

nonisolated enum LegKind: Hashable, Sendable {
    case walking
    case transit(TransitRide)
    case bikeShare(BikeShareRide)
}

nonisolated struct Guidance: Hashable, Sendable {
    let code: String
    let parameters: [String: String]

    var geometryIndex: Int? {
        parameters["geometry_index"].flatMap(Int.init)
    }

    var streetName: String? {
        parameters["street_name"].flatMap { $0.isEmpty ? nil : $0 }
    }
}

nonisolated struct Attribution: Hashable, Sendable {
    let text: String
    let url: URL?
    let licence: String
}

nonisolated struct Leg: Identifiable, Sendable {
    let id: String
    let kind: LegKind
    let from: Stop
    let to: Stop
    let departureTime: Date
    let arrivalTime: Date
    let duration: TimeInterval
    let path: [Coordinate]
    let guidance: [Guidance]
    let attributions: [Attribution]

    var transit: TransitRide? {
        if case let .transit(ride) = kind { ride } else { nil }
    }
}
