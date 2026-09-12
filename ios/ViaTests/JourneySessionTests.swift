import Foundation
import Testing
@testable import Via

@MainActor
struct JourneySessionTests {
    @Test("Submitting exposes server ordering and selects the first Journey Option")
    func submitSelectsFirstServerOption() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["first", "second"]))])
        let session = makeSession(planner: planner)

        await session.submit(locale: "en")

        #expect(session.state == .results)
        #expect(session.result?.options.map(\.id) == ["first", "second"])
        #expect(session.selectedOptionID == "first")
    }

    @Test("Refresh keeps the previous complete result visible when replacement fails")
    func refreshFailureKeepsResult() async {
        let initial = Self.result(ids: ["original"])
        let planner = PlannerStub(results: [.success(initial), .failure(.temporarilyUnavailable)])
        let session = makeSession(planner: planner)
        await session.submit(locale: "en")

        await session.refresh()

        #expect(session.state == .results)
        #expect(session.result?.options.map(\.id) == ["original"])
        #expect(session.refreshMessage != nil)
    }

    @Test("Refresh preserves selection only through the server matching key")
    func refreshUsesMatchingKey() async {
        let initial = Self.result(ids: ["a", "b"], matchingKeys: ["key-a", "key-b"])
        let replacement = Self.result(ids: ["new-b", "new-a"], matchingKeys: ["key-b", "key-a"])
        let planner = PlannerStub(results: [.success(initial), .success(replacement)])
        let session = makeSession(planner: planner)
        await session.submit(locale: "en")
        session.select(initial.options[1])

        await session.refresh()

        #expect(session.selectedOptionID == "new-b")
    }

    private func makeSession(planner: PlannerStub) -> JourneySession {
        let suite = UserDefaults(suiteName: UUID().uuidString)!
        return JourneySession(
            planner: planner,
            defaults: suite,
            origin: Self.place("Origin"),
            destination: Self.place("Destination")
        )
    }

    private static func place(_ name: String) -> SelectedPlace {
        SelectedPlace(displayText: name, coordinate: Coordinate(latitude: 25.04, longitude: 121.56))
    }

    private static func result(ids: [String], matchingKeys: [String]? = nil) -> JourneyResult {
        let keys = matchingKeys ?? ids.map { "key-\($0)" }
        let generatedAt = Date()
        return JourneyResult(
            apiVersion: "1.0",
            semanticCatalogVersion: "1.0",
            generatedAt: generatedAt,
            networkRevision: NetworkRevision(
                staticNetworkRevision: "static-1",
                scheduleRevision: "schedule-1"
            ),
            options: zip(ids, keys).map { id, key in JourneyOption(
                id: id,
                matchingKey: key,
                journeyClass: .reliable,
                label: .bestNow,
                departureTime: .now,
                arrivalTime: .now,
                durationSeconds: 600,
                walkingSeconds: 120,
                transferCount: 0,
                travelModes: [.walking, .transit],
                liveStatusSummary: LiveStatusSummary(state: .fresh, observedAt: .now),
                overview: [],
                legs: []
            ) }
        )
    }
}

private actor PlannerStub: JourneyPlanning {
    private var results: [Result<JourneyResult, JourneyPlanningError>]

    init(results: [Result<JourneyResult, JourneyPlanningError>]) {
        self.results = results
    }

    func plan(_ request: PlanningRequest) async throws -> JourneyResult {
        guard !results.isEmpty else { throw JourneyPlanningError.unexpectedFailure }
        return try results.removeFirst().get()
    }
}
