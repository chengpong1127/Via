import Foundation
import Testing
@testable import Via

struct NavigationModelTests {
    let destination = Place(name: "Taipei 101", coordinate: NavigationFixture.point(north: 400, east: 2200))

    @Test func reroutesFromTheCurrentPositionAndKeepsTheSameJourney() async throws {
        let planner = StubPlanner(plans: [plan(["other", "fixture"])])
        let model = makeModel(planner: planner)

        await leaveRoute(model, from: 10)

        let request = try #require(await planner.requests.first)
        #expect(request.origin.coordinate == NavigationFixture.point(north: 100, east: 100))
        #expect(request.destination == destination)
        #expect(request.modes == [.metro])
        #expect(request.departure == .now)
        #expect(model.option.id == "1-fixture")
        #expect(!model.rerouteFailed)
    }

    @Test func waitsForTheCooldownBeforeReroutingAgain() async {
        let planner = StubPlanner(plans: [plan(["fixture"]), plan(["fixture"])])
        let model = makeModel(planner: planner)

        await leaveRoute(model, from: 10)
        await leaveRoute(model, from: 20)

        #expect(await planner.requests.count == 1)
    }

    @Test func keepsTheRouteWhenReroutingFails() async {
        let model = makeModel(planner: StubPlanner(plans: [], error: .temporarilyUnavailable))

        await leaveRoute(model, from: 10)

        #expect(model.option.id == "fixture")
        #expect(model.rerouteFailed)
    }

    @Test func surfacesAlertsAsNotices() async {
        let model = makeModel(planner: StubPlanner(plans: []))

        await model.handle(NavigationFixture.fix(north: 0, east: 20, at: 200))

        #expect(model.notice?.alert == .hurry)
        model.dismissNotice()
        #expect(model.notice == nil)
    }

    private func leaveRoute(_ model: NavigationModel, from second: Int) async {
        for offset in 0..<3 {
            await model.handle(NavigationFixture.fix(north: 100, east: 100, at: second + offset))
        }
    }

    private func makeModel(planner: StubPlanner) -> NavigationModel {
        NavigationModel(
            option: NavigationFixture.option(),
            destination: destination,
            modes: [.metro],
            planner: planner,
            location: StubTracking(),
            clock: { NavigationFixture.t0.addingTimeInterval(30) }
        )
    }

    private func plan(_ keys: [String]) -> JourneyPlan {
        JourneyPlan(generatedAt: NavigationFixture.t0, options: keys.map { NavigationFixture.option(key: $0) })
    }
}

final class StubTracking: LocationTracking {
    func fixes() -> AsyncStream<LocationFix> { AsyncStream { $0.finish() } }
    func beginNavigation() {}
    func endNavigation() {}
}
