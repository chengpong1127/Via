import Foundation

enum PlanningMode: String, CaseIterable, Hashable, Identifiable, Sendable {
    case bus
    case metro
    case tra
    case thsr
    case bikeShare = "bike_share"

    var id: Self { self }

    static let selectable: [PlanningMode] = [.bus, .metro, .tra, .thsr, .bikeShare]

    var titleKey: String {
        switch self {
        case .bus: "Bus"
        case .metro: "Metro"
        case .tra: "TRA"
        case .thsr: "THSR"
        case .bikeShare: "Bike Share"
        }
    }

    var symbolName: String {
        switch self {
        case .bus: "bus.fill"
        case .metro: "tram.fill"
        case .tra, .thsr: "train.side.front.car"
        case .bikeShare: "bicycle"
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
    let selectedModes: Set<PlanningMode>
    let departureSetting: DepartureSetting
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
