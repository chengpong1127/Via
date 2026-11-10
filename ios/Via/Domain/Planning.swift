import Foundation

nonisolated enum PlanningMode: String, CaseIterable, Hashable, Identifiable, Sendable {
    case bus
    case metro
    case tra
    case thsr
    case bikeShare = "bike_share"

    var id: Self { self }
}

nonisolated enum DepartureSetting: Hashable, Sendable {
    case now
    case departAt(Date)
}

nonisolated struct PlanningRequest: Hashable, Sendable {
    var origin: Place
    var destination: Place
    var modes: Set<PlanningMode>
    var departure: DepartureSetting
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
