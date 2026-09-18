import CoreLocation
import Foundation

struct Coordinate: Hashable, Sendable {
    let latitude: Double
    let longitude: Double

    var clLocationCoordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

struct Place: Hashable, Sendable {
    let name: String
    let subtitle: String?
    let coordinate: Coordinate

    init(name: String, subtitle: String? = nil, coordinate: Coordinate) {
        self.name = name
        self.subtitle = subtitle
        self.coordinate = coordinate
    }

    var displayText: String { name }
}

enum JourneyEndpoint: Hashable, Sendable {
    case currentLocation
    case place(Place)
}

enum JourneyMode: String, CaseIterable, Hashable, Identifiable, Sendable {
    case walking
    case transit

    var id: Self { self }

    static let selectable: [JourneyMode] = [.transit]

    var titleKey: String {
        switch self {
        case .transit: "Transit"
        case .walking: "Walk"
        }
    }

    var symbolName: String {
        switch self {
        case .transit: "tram.fill"
        case .walking: "figure.walk"
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

enum DepartureSetting: Hashable, Sendable {
    case departNow
    case departAt(Date)
    case arriveBy(Date)
}

struct PlanningRequest: Hashable, Sendable {
    let origin: Place
    let destination: Place
    let selectedModes: Set<JourneyMode>
    let departureSetting: DepartureSetting
    let locale: String
}

enum JourneyClass: String, Equatable, Sendable {
    case reliable = "reliable"
    case feasible = "feasible"
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
    let serviceID: String
    let serviceLabel: String
    let headsign: String
    let boardingPointID: String
    let alightingPointID: String
}

struct JourneyLeg: Identifiable, Sendable {
    let id: String
    let mode: JourneyMode
    let title: String
    let subtitle: String
    let durationSeconds: Int
    let guidance: [GuidanceInstruction]
    let transit: TransitLegMetadata?

    init(
        id: String,
        mode: JourneyMode,
        title: String,
        subtitle: String,
        durationSeconds: Int,
        guidance: [GuidanceInstruction],
        transit: TransitLegMetadata? = nil
    ) {
        self.id = id
        self.mode = mode
        self.title = title
        self.subtitle = subtitle
        self.durationSeconds = durationSeconds
        self.guidance = guidance
        self.transit = transit
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
    let journeyClass: JourneyClass
    let label: JourneyLabel?
    let departureTime: Date
    let arrivalTime: Date
    let durationSeconds: Int
    let walkingSeconds: Int
    let transferCount: Int
    let travelModes: [JourneyMode]
    let liveStatusSummary: LiveStatusSummary
    let overview: [Coordinate]
    let legs: [JourneyLeg]
}

struct NetworkRevision: Sendable {
    let staticNetworkRevision: String
    let scheduleRevision: String
}

struct JourneyResult: Sendable {
    let apiVersion: String
    let semanticCatalogVersion: String
    let generatedAt: Date
    let networkRevision: NetworkRevision
    let options: [JourneyOption]
}

enum JourneyPlanningError: Error, Equatable, Sendable {
    case invalidRequest(field: String)
    case temporarilyUnavailable
    case unsupportedResponse
    case unexpectedFailure
}

enum LocationProvidingError: Error, Equatable, Sendable {
    case permissionDenied
    case restricted
    case unavailable
}

@MainActor
protocol LocationProviding {
    var authorizationStatus: CLAuthorizationStatus { get }

    func requestWhenInUseAuthorization()
    func currentLocation() async throws -> Coordinate
}

protocol JourneyPlanning: Sendable {
    func plan(_ request: PlanningRequest) async throws -> JourneyResult
}
