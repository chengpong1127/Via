import Foundation

nonisolated enum PlanningMode: String, CaseIterable, Hashable, Identifiable, Sendable {
    case bus
    case metro
    case tra
    case thsr
    case bikeShare = "bike_share"

    var id: Self { self }

    var transitMode: TransitMode? {
        switch self {
        case .bus: .bus
        case .metro: .metro
        case .tra: .rail
        case .thsr: .highSpeedRail
        case .bikeShare: nil
        }
    }
}

nonisolated struct PlanningRequest: Hashable, Sendable {
    var origin: Place
    var destination: Place
    var modes: Set<PlanningMode>
}

nonisolated enum JourneyPlanningError: Error, Equatable, Sendable {
    case invalidRequest(field: String?)
    case timedOut
    case temporarilyUnavailable
    case serverUnreachable
    case unsupportedResponse
    case unexpected
}

nonisolated protocol JourneyPlanner: Sendable {
    func plan(_ request: PlanningRequest) async throws -> JourneyPlan
}
