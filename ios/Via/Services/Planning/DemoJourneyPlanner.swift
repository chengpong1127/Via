import Foundation

nonisolated struct DemoJourneyPlanner: JourneyPlanner {
    var latency: Duration = .milliseconds(600)
    var now: @Sendable () -> Date = { Date() }

    func plan(_ request: PlanningRequest) async throws -> JourneyPlan {
        try await Task.sleep(for: latency)
        let start: Date = switch request.departure {
        case .now: now()
        case let .departAt(date): date
        }
        var options: [JourneyOption] = []
        if request.modes.contains(.metro) {
            options.append(transitOption(request, start: start, mode: .metro, label: "BL Bannan Line", headsign: "Nangang Exhibition Center", ride: 14))
        }
        if request.modes.contains(.bus) {
            options.append(transitOption(request, start: start, mode: .bus, label: "307", headsign: "Banqiao", ride: 22))
        }
        if request.modes.contains(.bikeShare) {
            options.append(bikeOption(request, start: start))
        }
        options.append(walkingOption(request, start: start))
        return JourneyPlan(generatedAt: now(), options: options)
    }

    private func walkingOption(_ request: PlanningRequest, start: Date) -> JourneyOption {
        let distance = request.origin.coordinate.distance(to: request.destination.coordinate)
        let duration = max(60, (distance * 1.3 / 1.25).rounded())
        let walk = leg("walk", .walking, request.origin.stop, request.destination.stop, start, duration)
        return option("walking", label: .lessWalking, legs: [walk], walking: duration)
    }

    private func transitOption(_ request: PlanningRequest, start: Date, mode: TransitMode, label: String, headsign: String, ride: Double) -> JourneyOption {
        let board = request.origin.coordinate.interpolated(to: request.destination.coordinate, fraction: 0.12)
        let alight = request.origin.coordinate.interpolated(to: request.destination.coordinate, fraction: 0.9)
        let boardStop = Stop(name: mode == .metro ? "Taipei Main Station" : "Zhongxiao W. Rd.", coordinate: board, viaID: nil)
        let alightStop = Stop(name: mode == .metro ? "Taipei City Hall" : "Xinyi Anhe", coordinate: alight, viaID: nil)
        let first = leg("\(mode)-walk-1", .walking, request.origin.stop, boardStop, start, 300)
        let transit = leg(
            "\(mode)-ride",
            .transit(TransitRide(
                mode: mode,
                serviceLabel: label,
                headsign: headsign,
                patternID: "demo-\(mode)",
                intermediateStops: [0.3, 0.5, 0.7].enumerated().map { index, fraction in
                    Stop(name: "Stop \(index + 1)", coordinate: board.interpolated(to: alight, fraction: fraction), viaID: nil)
                }
            )),
            boardStop, alightStop, first.arrivalTime.addingTimeInterval(120), ride * 60
        )
        let last = leg("\(mode)-walk-2", .walking, alightStop, request.destination.stop, transit.arrivalTime, 240)
        return option(
            "\(mode)",
            label: mode == .metro ? .bestNow : .fewerTransfers,
            legs: [first, transit, last],
            walking: 540,
            live: mode == .bus ? .fresh : .notApplicable
        )
    }

    private func bikeOption(_ request: PlanningRequest, start: Date) -> JourneyOption {
        let pickup = Stop(name: "YouBike MRT Station Exit 1", coordinate: request.origin.coordinate.interpolated(to: request.destination.coordinate, fraction: 0.05), viaID: nil)
        let dropoff = Stop(name: "YouBike City Hall", coordinate: request.origin.coordinate.interpolated(to: request.destination.coordinate, fraction: 0.95), viaID: nil)
        let first = leg("bike-walk-1", .walking, request.origin.stop, pickup, start, 120)
        let ride = leg(
            "bike-ride",
            .bikeShare(BikeShareRide(pickupStationID: "p", pickupStationName: pickup.name, dropoffStationID: "d", dropoffStationName: dropoff.name)),
            pickup, dropoff, first.arrivalTime, 17 * 60
        )
        let last = leg("bike-walk-2", .walking, dropoff, request.destination.stop, ride.arrivalTime, 120)
        return option("bike", label: nil, legs: [first, ride, last], walking: 240, live: .fresh)
    }

    private func leg(_ id: String, _ kind: LegKind, _ from: Stop, _ to: Stop, _ start: Date, _ duration: TimeInterval) -> Leg {
        Leg(
            id: id,
            kind: kind,
            from: from,
            to: to,
            departureTime: start,
            arrivalTime: start.addingTimeInterval(duration),
            duration: duration,
            path: (0...8).map { from.coordinate.interpolated(to: to.coordinate, fraction: Double($0) / 8) },
            guidance: guidance(for: kind),
            attributions: []
        )
    }

    private func guidance(for kind: LegKind) -> [Guidance] {
        func step(_ code: String, _ index: Int, _ street: String? = nil) -> Guidance {
            var parameters = ["geometry_index": String(index)]
            parameters["street_name"] = street
            return Guidance(code: code, parameters: parameters)
        }
        switch kind {
        case .walking:
            return [step("depart", 0), step("turn_right", 3, "Zhongxiao E. Rd."), step("turn_left", 6, "Songren Rd."), step("arrive", 8)]
        case .bikeShare:
            return [Guidance(code: "bike_share", parameters: [:]), step("depart", 0), step("turn_left", 4, "Xinyi Rd."), step("arrive", 8)]
        case .transit:
            return []
        }
    }

    private func option(_ key: String, label: JourneyLabel?, legs: [Leg], walking: TimeInterval, live: LiveStatus = .notApplicable) -> JourneyOption {
        let departure = legs.first!.departureTime
        let arrival = legs.last!.arrivalTime
        return JourneyOption(
            id: "demo-\(key)",
            matchingKey: key,
            label: label,
            departureTime: departure,
            arrivalTime: arrival,
            duration: arrival.timeIntervalSince(departure),
            walkingDuration: walking,
            transferCount: 0,
            liveStatus: live,
            liveObservedAt: live == .fresh ? now() : nil,
            overview: legs.flatMap(\.path),
            legs: legs
        )
    }
}

private nonisolated extension Place {
    var stop: Stop { Stop(name: name, coordinate: coordinate, viaID: nil) }
}

private nonisolated extension Coordinate {
    func interpolated(to other: Coordinate, fraction: Double) -> Coordinate {
        Coordinate(
            latitude: latitude + (other.latitude - latitude) * fraction,
            longitude: longitude + (other.longitude - longitude) * fraction
        )
    }
}
