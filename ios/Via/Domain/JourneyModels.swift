import Foundation

enum LegMode: String, CaseIterable, Hashable, Identifiable, Sendable {
    case walking
    case transit
    case bicycle

    var id: Self { self }

    var titleKey: String {
        switch self {
        case .transit: "Transit"
        case .walking: "Walk"
        case .bicycle: "Bike"
        }
    }

    var symbolName: String {
        switch self {
        case .transit: "tram.fill"
        case .walking: "figure.walk"
        case .bicycle: "bicycle"
        }
    }
}

enum TransitMode: String, CaseIterable, Hashable, Sendable {
    case bus
    case metro
    case rail
    case highSpeedRail = "high_speed_rail"

    var titleKey: String {
        switch self {
        case .bus: "Bus"
        case .metro: "Metro"
        case .rail: "Rail"
        case .highSpeedRail: "High-speed rail"
        }
    }

    var symbolName: String {
        switch self {
        case .bus: "bus.fill"
        case .metro: "tram.fill"
        case .rail: "train.side.front.car"
        case .highSpeedRail: "train.side.front.car"
        }
    }
}

enum JourneyLabel: String, Equatable, Sendable {
    case bestNow = "best_now"
    case fewerTransfers = "fewer_transfers"
    case lessWalking = "less_walking"

    var titleKey: String {
        switch self {
        case .bestNow: "Best now"
        case .fewerTransfers: "Fewer transfers"
        case .lessWalking: "Less walking"
        }
    }
}

enum LiveStatusState: String, Equatable, Sendable {
    case fresh = "fresh"
    case degraded = "degraded"
    case unavailable = "unavailable"
    case notApplicable = "not_applicable"
}

struct LiveStatusSummary: Sendable {
    let state: LiveStatusState
    let observedAt: Date?
}

struct GuidanceInstruction: Identifiable, Sendable {
    let id = UUID()
    let title: String
    let detail: String?
}

struct TransitLegMetadata: Sendable {
    let mode: TransitMode
    let patternID: String
    let serviceLabel: String
    let headsign: String
    let boardingPointID: String
    let alightingPointID: String
}

struct BikeShareLegMetadata: Sendable {
    let pickupStationID: String
    let pickupStationName: String
    let dropoffStationID: String
    let dropoffStationName: String
}

struct JourneyLeg: Identifiable, Sendable {
    let id: String
    let mode: LegMode
    let title: String
    let subtitle: String
    let durationSeconds: Int
    let guidance: [GuidanceInstruction]
    let transit: TransitLegMetadata?
    let bikeShare: BikeShareLegMetadata?

    init(
        id: String,
        mode: LegMode,
        title: String,
        subtitle: String,
        durationSeconds: Int,
        guidance: [GuidanceInstruction],
        transit: TransitLegMetadata? = nil,
        bikeShare: BikeShareLegMetadata? = nil
    ) {
        self.id = id
        self.mode = mode
        self.title = title
        self.subtitle = subtitle
        self.durationSeconds = durationSeconds
        self.guidance = guidance
        self.transit = transit
        self.bikeShare = bikeShare
    }

    var symbolName: String {
        if mode == .transit, let transit {
            return transit.mode.symbolName
        }
        return mode.symbolName
    }
}

struct JourneyOption: Identifiable, Sendable {
    let id: String
    let matchingKey: String
    let label: JourneyLabel?
    let departureTime: Date
    let arrivalTime: Date
    let durationSeconds: Int
    let walkingSeconds: Int
    let transferCount: Int
    let travelModes: [LegMode]
    let liveStatusSummary: LiveStatusSummary
    let overview: [Coordinate]
    let legs: [JourneyLeg]

}

struct JourneyResult: Sendable {
    let generatedAt: Date
    let options: [JourneyOption]
}
