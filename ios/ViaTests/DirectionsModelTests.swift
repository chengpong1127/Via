import Foundation
import Testing
@testable import Via

struct DirectionsModelTests {
    let destination = Place(name: "Taipei 101", coordinate: Coordinate(latitude: 25.03396, longitude: 121.56447))

    @Test func plansFromCurrentLocationAndSelectsFirstOption() async throws {
        let planner = StubPlanner(plans: [plan(["a", "b"])])
        let model = makeModel(planner: planner)
        model.destination = .place(destination)

        await model.plan()

        #expect(model.options.map(\.matchingKey) == ["a", "b"])
        #expect(model.selectedOption?.matchingKey == "a")
        let sent = try #require(await planner.requests.first)
        #expect(sent.origin.coordinate == StubLocation.taipeiMain)
        #expect(sent.origin.name == String(localized: "My Location"))
        #expect(sent.destination == destination)
    }

    @Test func keepsSelectedRouteAcrossReplanning() async {
        let planner = StubPlanner(plans: [plan(["a", "b"]), plan(["c", "b"])])
        let model = makeModel(planner: planner)
        model.destination = .place(destination)
        await model.plan()
        model.selectedOptionID = model.options[1].id

        await model.plan()

        #expect(model.selectedOption?.matchingKey == "b")
        #expect(model.selectedOption?.id == "2-b")
    }

    @Test func reportsDeniedLocation() async {
        let model = makeModel(planner: StubPlanner(plans: []), location: StubLocation(error: .denied))
        model.destination = .place(destination)

        await model.plan()

        guard case .failed(.location(.denied)) = model.phase else {
            Issue.record("Expected a location failure, got \(model.phase)")
            return
        }
    }

    @Test func reportsPlanningFailure() async {
        let model = makeModel(planner: StubPlanner(plans: [], error: .temporarilyUnavailable))
        model.destination = .place(destination)

        await model.plan()

        guard case .failed(.planning(.temporarilyUnavailable)) = model.phase else {
            Issue.record("Expected a planning failure, got \(model.phase)")
            return
        }
    }

    @Test func rejectsIdenticalEndpoints() async {
        let planner = StubPlanner(plans: [plan(["a"])])
        let model = makeModel(planner: planner)
        model.origin = .place(destination)
        model.destination = .place(destination)

        await model.plan()

        guard case .failed(.sameEndpoints) = model.phase else {
            Issue.record("Expected same endpoints failure")
            return
        }
        #expect(await planner.requests.isEmpty)
    }

    @Test func ignoresStaleResponses() async {
        let planner = StubPlanner(plans: [plan(["slow"]), plan(["fast"])], delays: [.milliseconds(300), .zero])
        let model = makeModel(planner: planner)
        model.destination = .place(destination)

        async let first: Void = model.plan()
        try? await Task.sleep(for: .milliseconds(50))
        await model.plan()
        await first

        #expect(model.options.map(\.matchingKey) == ["fast"])
    }

    @Test func persistsSelectedModes() {
        let defaults = UserDefaults(suiteName: "DirectionsModelTests.\(UUID().uuidString)")!
        let model = DirectionsModel(planner: StubPlanner(plans: []), location: StubLocation(), defaults: defaults)
        #expect(model.modes == Set(PlanningMode.allCases))

        model.modes.subtract([.bus, .thsr])

        let restored = DirectionsModel(planner: StubPlanner(plans: []), location: StubLocation(), defaults: defaults)
        #expect(restored.modes == [.metro, .tra, .bikeShare])
    }

    @Test func swapsEndpoints() {
        let model = makeModel(planner: StubPlanner(plans: []))
        model.destination = .place(destination)
        model.swapEndpoints()
        #expect(model.origin == .place(destination))
        #expect(model.destination == .currentLocation)
    }

    private func makeModel(planner: StubPlanner, location: StubLocation = StubLocation()) -> DirectionsModel {
        DirectionsModel(
            planner: planner,
            location: location,
            defaults: UserDefaults(suiteName: "DirectionsModelTests.\(UUID().uuidString)")!
        )
    }

    private func plan(_ keys: [String]) -> JourneyPlan {
        let now = Date()
        return JourneyPlan(generatedAt: now, options: keys.map { key in
            JourneyOption(
                id: key,
                matchingKey: key,
                departureTime: now,
                arrivalTime: now.addingTimeInterval(600),
                duration: 600,
                walkingDuration: 600,
                transferCount: 0,
                liveStatus: .notApplicable,
                liveObservedAt: nil,
                overview: [],
                legs: []
            )
        })
    }
}

actor StubPlanner: JourneyPlanner {
    private var plans: [JourneyPlan]
    private var delays: [Duration]
    private let error: JourneyPlanningError?
    private(set) var requests: [PlanningRequest] = []

    init(plans: [JourneyPlan], delays: [Duration] = [], error: JourneyPlanningError? = nil) {
        self.plans = plans
        self.delays = delays
        self.error = error
    }

    func plan(_ request: PlanningRequest) async throws -> JourneyPlan {
        requests.append(request)
        let call = requests.count
        if let error { throw error }
        let plan = plans.removeFirst()
        if !delays.isEmpty { try await Task.sleep(for: delays.removeFirst()) }
        return JourneyPlan(generatedAt: plan.generatedAt, options: plan.options.map { option in
            JourneyOption(
                id: "\(call)-\(option.id)",
                matchingKey: option.matchingKey,
                departureTime: option.departureTime,
                arrivalTime: option.arrivalTime,
                duration: option.duration,
                walkingDuration: option.walkingDuration,
                transferCount: option.transferCount,
                liveStatus: option.liveStatus,
                liveObservedAt: option.liveObservedAt,
                overview: option.overview,
                legs: option.legs
            )
        })
    }
}

final class StubLocation: LocationProviding {
    static let taipeiMain = Coordinate(latitude: 25.04776, longitude: 121.51706)
    let error: LocationError?

    init(error: LocationError? = nil) {
        self.error = error
    }

    func currentCoordinate() async throws -> Coordinate {
        if let error { throw error }
        return Self.taipeiMain
    }
}
