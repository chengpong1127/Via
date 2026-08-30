import CoreLocation
import Foundation

struct Coordinate: Hashable, Sendable {
    let latitude: Double
    let longitude: Double

    var clLocationCoordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

struct SelectedPlace: Hashable, Identifiable, Sendable {
    let id = UUID()
    let displayText: String
    let coordinate: Coordinate
}

enum TravelMode: String, CaseIterable, Hashable, Identifiable, Sendable {
    case bus
    case metro
    case youbike
    case walking

    var id: Self { self }

    static let selectable: [TravelMode] = [.bus, .metro, .youbike]

    var titleKey: String {
        switch self {
        case .bus: "Bus"
        case .metro: "Metro"
        case .youbike: "YouBike"
        case .walking: "Walk"
        }
    }

    var symbolName: String {
        switch self {
        case .bus: "bus.fill"
        case .metro: "tram.fill"
        case .youbike: "bicycle"
        case .walking: "figure.walk"
        }
    }
}

enum DepartureSetting: Hashable, Sendable {
    case departNow
    case departAt(Date)
    case arriveBy(Date)
}

struct PlanningRequest: Hashable, Sendable {
    let origin: SelectedPlace
    let destination: SelectedPlace
    let selectedModes: Set<TravelMode>
    let departureSetting: DepartureSetting
    let locale: String
}

enum JourneyClass: Sendable {
    case reliable
    case feasible
}

enum JourneyLabel: Sendable {
    case bestNow
    case fewerTransfers
    case lessWalking

    var titleKey: String {
        switch self {
        case .bestNow: "Best now"
        case .fewerTransfers: "Fewer transfers"
        case .lessWalking: "Less walking"
        }
    }
}

enum LiveStatusState: Sendable {
    case fresh
    case degraded
    case unavailable
    case notApplicable
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

struct JourneyLeg: Identifiable, Sendable {
    let id: String
    let mode: TravelMode
    let title: String
    let subtitle: String
    let durationSeconds: Int
    let guidance: [GuidanceInstruction]
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
    let travelModes: [TravelMode]
    let liveStatusSummary: LiveStatusSummary
    let overview: [Coordinate]
    let legs: [JourneyLeg]
}

struct NetworkSnapshot: Sendable {
    let id: String
    let createdAt: Date
    let liveCutoff: Date
    let staticNetworkRevision: String
    let scheduleRevision: String
}

struct JourneyResult: Sendable {
    let apiVersion: String
    let semanticCatalogVersion: String
    let generatedAt: Date
    let networkSnapshot: NetworkSnapshot
    let options: [JourneyOption]
}

enum JourneyPlanningError: Error, Equatable, Sendable {
    case invalidRequest(field: String)
    case temporarilyUnavailable
    case unsupportedResponse
    case unexpectedFailure
}

protocol JourneyPlanning: Sendable {
    func plan(_ request: PlanningRequest) async throws -> JourneyResult
}
