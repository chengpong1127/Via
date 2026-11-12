import Foundation
import Testing
@testable import Via

struct NavigationTrackerTests {
    let t0 = NavigationFixture.t0

    @Test func placesManeuversAtGeometryIndices() {
        let route = NavigationRoute(option: NavigationFixture.option())
        let walk = route.legs[0]
        #expect(walk.maneuvers.map(\.guidance.code) == ["turn_left", "arrive"])
        #expect(abs(walk.maneuvers[0].distanceAlong - 200) < 2)
        #expect(abs(walk.maneuvers[1].distanceAlong - 400) < 2)
        #expect(route.legs[1].stops.count == 5)
    }

    @Test func addsArrivalWhenGuidanceHasNoPositions() {
        let route = NavigationRoute(option: NavigationFixture.option(walkGuidance: []))
        #expect(route.legs[0].maneuvers.map(\.guidance.code) == ["arrive"])
    }

    @Test func reportsDistanceToNextManeuver() {
        var tracker = makeTracker()
        _ = tracker.update(fix(north: 5, east: 100, at: 10))
        #expect(tracker.nextManeuver?.guidance.code == "turn_left")
        #expect(tracker.nextManeuver?.guidance.streetName == "Zhongxiao E. Rd.")
        #expect(abs((tracker.distanceToManeuver ?? 0) - 100) < 3)
    }

    @Test func neverMovesBackwards() {
        var tracker = makeTracker()
        _ = tracker.update(fix(north: 0, east: 150, at: 10))
        _ = tracker.update(fix(north: 0, east: 100, at: 12))
        #expect(abs(tracker.distanceAlong - 150) < 3)
    }

    @Test func flagsOffRouteAfterConsecutiveFixes() {
        var tracker = makeTracker()
        #expect(tracker.update(fix(north: 100, east: 100, at: 10)).reroute == nil)
        #expect(tracker.update(fix(north: 100, east: 100, at: 11)).reroute == nil)
        #expect(tracker.update(fix(north: 100, east: 100, at: 12)).reroute == .offRoute)
    }

    @Test func ignoresInaccurateFixes() {
        var tracker = makeTracker()
        for second in 10...14 {
            #expect(tracker.update(fix(north: 100, east: 100, at: second, accuracy: 250)).reroute == nil)
        }
    }

    @Test func advancesToTheNextLegNearTheEnd() {
        var tracker = makeTracker()
        _ = tracker.update(fix(north: 0, east: 200, at: 10))
        _ = tracker.update(fix(north: 190, east: 200, at: 20))
        #expect(tracker.legIndex == 1)
    }

    @Test func countsDownStopsAndAdvancesAtTheAlightingStop() {
        var tracker = trackerOnTransit()
        _ = tracker.update(fix(north: 200, east: 900, at: 450))
        #expect(tracker.boarded)
        #expect(tracker.stopsRemaining == 3)

        #expect(tracker.update(fix(north: 200, east: 1300, at: 600)).alerts == [.stopsLeft(2)])
        #expect(tracker.update(fix(north: 200, east: 1800, at: 700)).alerts == [.nextStop("City Hall")])
        let last = tracker.update(fix(north: 200, east: 2160, at: 900))
        #expect(last.alerts == [.getOff("City Hall")])
        #expect(tracker.legIndex == 2)
    }

    @Test func detectsMissedConnection() {
        var tracker = trackerOnTransit()
        let update = tracker.update(fix(north: 205, east: 210, at: 600))
        #expect(update.reroute == .missedConnection)
    }

    @Test func fallsBackToTimetableWithoutFixes() {
        var tracker = trackerOnTransit()
        let update = tracker.tick(now: t0.addingTimeInterval(950))
        #expect(tracker.boarded)
        #expect(update.alerts == [.nextStop("City Hall")])
    }

    @Test func warnsWhenTheWalkIsTooSlowForTheConnection() {
        var tracker = makeTracker()
        let update = tracker.update(fix(north: 0, east: 20, at: 200))
        #expect(update.alerts == [.hurry])
    }

    @Test func finishesAtTheDestination() {
        var tracker = trackerOnTransit()
        _ = tracker.update(fix(north: 200, east: 900, at: 450))
        _ = tracker.update(fix(north: 200, east: 2190, at: 900))
        let update = tracker.update(fix(north: 395, east: 2200, at: 1100))
        #expect(tracker.isFinished)
        #expect(update.alerts == [.arrived])
    }

    private func makeTracker() -> NavigationTracker {
        NavigationTracker(route: NavigationRoute(option: NavigationFixture.option()), startedAt: t0)
    }

    private func trackerOnTransit() -> NavigationTracker {
        var tracker = makeTracker()
        _ = tracker.update(fix(north: 0, east: 200, at: 10))
        _ = tracker.update(fix(north: 195, east: 200, at: 20))
        return tracker
    }

    private func fix(north: Double, east: Double, at second: Int, accuracy: Double = 5) -> LocationFix {
        NavigationFixture.fix(north: north, east: east, at: second, accuracy: accuracy)
    }
}

enum NavigationFixture {
    static let t0 = Date(timeIntervalSince1970: 1_793_500_000)
    static let base = Coordinate(latitude: 25.04, longitude: 121.52)

    static func point(north: Double, east: Double) -> Coordinate {
        Coordinate(
            latitude: base.latitude + north / 110_540,
            longitude: base.longitude + east / (111_320 * cos(base.latitude * .pi / 180))
        )
    }

    static func fix(north: Double, east: Double, at second: Int, accuracy: Double = 5) -> LocationFix {
        LocationFix(
            coordinate: point(north: north, east: east),
            horizontalAccuracy: accuracy,
            timestamp: t0.addingTimeInterval(TimeInterval(second))
        )
    }

    static func option(key: String = "fixture", walkGuidance: [Guidance]? = nil) -> JourneyOption {
        let start = stop("Start", north: 0, east: 0)
        let boarding = stop("Taipei Main Station", north: 200, east: 200)
        let alighting = stop("City Hall", north: 200, east: 2200)
        let end = stop("Taipei 101", north: 400, east: 2200)
        let guidance = walkGuidance ?? [
            Guidance(code: "depart", parameters: ["geometry_index": "0"]),
            Guidance(code: "turn_left", parameters: ["geometry_index": "1", "street_name": "Zhongxiao E. Rd."]),
            Guidance(code: "arrive", parameters: ["geometry_index": "2"]),
        ]
        let legs = [
            Leg(
                id: "walk-1", kind: .walking, from: start, to: boarding,
                departureTime: t0, arrivalTime: t0.addingTimeInterval(300), duration: 300,
                path: [start.coordinate, point(north: 0, east: 200), boarding.coordinate],
                guidance: guidance, attributions: []
            ),
            Leg(
                id: "ride", kind: .transit(TransitRide(
                    mode: .metro, serviceLabel: "BL", headsign: "Nangang", patternID: "p",
                    intermediateStops: [700, 1200, 1700].map { stop("Stop \(Int($0))", north: 200, east: $0) }
                )),
                from: boarding, to: alighting,
                departureTime: t0.addingTimeInterval(400), arrivalTime: t0.addingTimeInterval(1000), duration: 600,
                path: [boarding.coordinate, alighting.coordinate], guidance: [], attributions: []
            ),
            Leg(
                id: "walk-2", kind: .walking, from: alighting, to: end,
                departureTime: t0.addingTimeInterval(1000), arrivalTime: t0.addingTimeInterval(1200), duration: 200,
                path: [alighting.coordinate, end.coordinate], guidance: [], attributions: []
            ),
        ]
        return JourneyOption(
            id: key, matchingKey: key,
            departureTime: t0, arrivalTime: t0.addingTimeInterval(1200), duration: 1200,
            walkingDuration: 500, transferCount: 0, liveStatus: .notApplicable, liveObservedAt: nil,
            overview: legs.flatMap(\.path), legs: legs
        )
    }

    private static func stop(_ name: String, north: Double, east: Double) -> Stop {
        Stop(name: name, coordinate: point(north: north, east: east), viaID: nil)
    }
}
