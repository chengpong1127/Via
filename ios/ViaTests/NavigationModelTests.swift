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

    @Test func reportsTheActualArrivalTimeOnceFinished() async {
        let model = makeModel(planner: StubPlanner(plans: []))

        await model.handle(NavigationFixture.fix(north: 0, east: 200, at: 10))
        await model.handle(NavigationFixture.fix(north: 195, east: 200, at: 20))
        await model.handle(NavigationFixture.fix(north: 200, east: 900, at: 450))
        await model.handle(NavigationFixture.fix(north: 200, east: 2190, at: 900))
        await model.handle(NavigationFixture.fix(north: 395, east: 2200, at: 1100))

        #expect(model.isFinished)
        #expect(model.estimatedArrival == NavigationFixture.t0.addingTimeInterval(1100))
    }

    @Test func announcesATurnOnceWhenItIsNear() async {
        let feedback = RecordingFeedback()
        let model = makeModel(planner: StubPlanner(plans: []), feedback: feedback)

        await model.handle(NavigationFixture.fix(north: 0, east: 100, at: 10))
        #expect(feedback.cues.isEmpty)
        await model.handle(NavigationFixture.fix(north: 0, east: 175, at: 40))
        await model.handle(NavigationFixture.fix(north: 0, east: 180, at: 45))

        #expect(feedback.cues == [.maneuver("Turn left onto Zhongxiao E. Rd.")])
    }

    @Test func sendsAlertsAndRerouteResultsAsCues() async {
        let feedback = RecordingFeedback()
        let model = makeModel(planner: StubPlanner(plans: [plan(["fixture"])]), feedback: feedback)

        await model.handle(NavigationFixture.fix(north: 0, east: 20, at: 200))
        await leaveRoute(model, from: 210)

        #expect(feedback.cues == [.alert(.hurry), .rerouted])
    }

    @Test func reportsAFailedReroute() async {
        let feedback = RecordingFeedback()
        let model = makeModel(planner: StubPlanner(plans: [], error: .temporarilyUnavailable), feedback: feedback)

        await leaveRoute(model, from: 10)

        #expect(feedback.cues == [.rerouteFailed])
    }

    @Test func describesTheCurrentStage() async {
        let feedback = RecordingFeedback()
        let model = makeModel(planner: StubPlanner(plans: []), feedback: feedback)

        await model.handle(NavigationFixture.fix(north: 0, east: 5, at: 10))
        #expect(feedback.statuses.last?.title == "Turn left onto Zhongxiao E. Rd.")

        await model.handle(NavigationFixture.fix(north: 0, east: 200, at: 20))
        await model.handle(NavigationFixture.fix(north: 195, east: 200, at: 30))
        #expect(feedback.statuses.last?.title == "Board BL at Taipei Main Station")

        await model.handle(NavigationFixture.fix(north: 200, east: 900, at: 450))
        #expect(feedback.statuses.last?.title == "3 stops to City Hall")
    }

    @Test func startsAndStopsFeedbackOnce() async {
        let feedback = RecordingFeedback()
        let model = makeModel(planner: StubPlanner(plans: []), feedback: feedback)

        model.start()
        await model.handle(NavigationFixture.fix(north: 0, east: 200, at: 10))
        await model.handle(NavigationFixture.fix(north: 195, east: 200, at: 20))
        await model.handle(NavigationFixture.fix(north: 200, east: 900, at: 450))
        await model.handle(NavigationFixture.fix(north: 200, east: 2190, at: 900))
        await model.handle(NavigationFixture.fix(north: 395, east: 2200, at: 1100))
        model.stop()

        #expect(feedback.starts == ["Taipei 101"])
        #expect(feedback.stops.count == 1)
        #expect(feedback.stops.first?.isFinished == true)
        #expect(feedback.cues.last == .alert(.arrived))
    }

    private func leaveRoute(_ model: NavigationModel, from second: Int) async {
        for offset in 0..<3 {
            await model.handle(NavigationFixture.fix(north: 100, east: 100, at: second + offset))
        }
    }

    private func makeModel(planner: StubPlanner, feedback: RecordingFeedback? = nil) -> NavigationModel {
        NavigationModel(
            option: NavigationFixture.option(),
            destination: destination,
            modes: [.metro],
            planner: planner,
            location: StubTracking(),
            feedback: feedback.map { [$0] } ?? [],
            clock: { NavigationFixture.t0.addingTimeInterval(30) }
        )
    }

    private func plan(_ keys: [String]) -> JourneyPlan {
        JourneyPlan(generatedAt: NavigationFixture.t0, options: keys.map { NavigationFixture.option(key: $0) })
    }
}

final class RecordingFeedback: NavigationFeedback {
    private(set) var starts: [String] = []
    private(set) var statuses: [NavigationStatus] = []
    private(set) var cues: [NavigationCue] = []
    private(set) var stops: [NavigationStatus] = []

    func start(destination: String, status: NavigationStatus) { starts.append(destination) }
    func update(_ status: NavigationStatus) { statuses.append(status) }
    func cue(_ cue: NavigationCue) { cues.append(cue) }
    func stop(_ status: NavigationStatus) { stops.append(status) }
}

final class StubTracking: LocationTracking {
    func fixes() -> AsyncStream<LocationFix> { AsyncStream { $0.finish() } }
    func beginNavigation() {}
    func endNavigation() {}
}
