import Foundation

nonisolated struct NavigationRoute: Sendable {
    let option: JourneyOption
    let legs: [NavigationLeg]

    init(option: JourneyOption) {
        self.option = option
        legs = option.legs.map(NavigationLeg.init)
    }
}

nonisolated struct NavigationManeuver: Hashable, Sendable {
    let guidance: Guidance
    let distanceAlong: Double
    let coordinate: Coordinate
}

nonisolated struct NavigationLeg: Sendable {
    let leg: Leg
    let path: [Coordinate]
    let cumulative: [Double]
    let maneuvers: [NavigationManeuver]
    let stops: [Stop]
    let stopDistances: [Double]

    init(_ leg: Leg) {
        self.leg = leg
        switch leg.kind {
        case let .transit(ride):
            let stops = [leg.from] + ride.intermediateStops + [leg.to]
            let path = stops.map(\.coordinate)
            let cumulative = Self.cumulative(path)
            self.path = path
            self.cumulative = cumulative
            self.stops = stops
            stopDistances = cumulative
            maneuvers = []
        case .walking, .bikeShare:
            let path = leg.path.count >= 2 ? leg.path : [leg.from.coordinate, leg.to.coordinate]
            let cumulative = Self.cumulative(path)
            self.path = path
            self.cumulative = cumulative
            stops = []
            stopDistances = []
            maneuvers = Self.maneuvers(leg.guidance, path: path, cumulative: cumulative)
        }
    }

    var length: Double { cumulative.last ?? 0 }

    func split(at distance: Double) -> (traveled: [Coordinate], remaining: [Coordinate]) {
        guard path.count >= 2, distance > 0 else { return ([], path) }
        guard distance < length else { return (path, []) }
        let index = cumulative.lastIndex { $0 <= distance } ?? 0
        let start = path[index]
        let end = path[index + 1]
        let span = cumulative[index + 1] - cumulative[index]
        let ratio = span > 0 ? (distance - cumulative[index]) / span : 0
        let point = Coordinate(
            latitude: start.latitude + (end.latitude - start.latitude) * ratio,
            longitude: start.longitude + (end.longitude - start.longitude) * ratio
        )
        return (Array(path[...index]) + [point], [point] + Array(path[(index + 1)...]))
    }

    var isStreet: Bool {
        if case .transit = leg.kind { false } else { true }
    }

    private static func cumulative(_ path: [Coordinate]) -> [Double] {
        var total = 0.0
        var result = [0.0]
        for (a, b) in zip(path, path.dropFirst()) {
            total += a.distance(to: b)
            result.append(total)
        }
        return result
    }

    private static func maneuvers(_ guidance: [Guidance], path: [Coordinate], cumulative: [Double]) -> [NavigationManeuver] {
        let ignored: Set<String> = ["depart", "bike_share"]
        var result = guidance.compactMap { item -> NavigationManeuver? in
            guard !ignored.contains(item.code), let index = item.geometryIndex else { return nil }
            let clamped = min(max(index, 0), path.count - 1)
            return NavigationManeuver(guidance: item, distanceAlong: cumulative[clamped], coordinate: path[clamped])
        }
        if result.last?.guidance.code != "arrive", let end = path.last {
            result.append(NavigationManeuver(
                guidance: Guidance(code: "arrive", parameters: [:]),
                distanceAlong: cumulative.last ?? 0,
                coordinate: end
            ))
        }
        return result
    }
}

nonisolated struct PathProjection: Equatable, Sendable {
    let distanceAlong: Double
    let offset: Double
}

nonisolated extension NavigationLeg {
    func project(_ coordinate: Coordinate, from start: Double = 0, through end: Double = .infinity) -> PathProjection? {
        guard path.count >= 2 else { return nil }
        var best: PathProjection?
        for index in 0..<(path.count - 1) {
            let segmentStart = cumulative[index]
            let segmentEnd = cumulative[index + 1]
            guard segmentEnd >= start, segmentStart <= end else { continue }
            let (fraction, offset) = Self.segmentProjection(coordinate, path[index], path[index + 1])
            let along = segmentStart + (segmentEnd - segmentStart) * fraction
            if best == nil || offset < best!.offset {
                best = PathProjection(distanceAlong: along, offset: offset)
            }
        }
        return best
    }

    private static func segmentProjection(_ point: Coordinate, _ a: Coordinate, _ b: Coordinate) -> (Double, Double) {
        let metresPerDegreeLatitude = 110_540.0
        let metresPerDegreeLongitude = 111_320.0 * cos(a.latitude * .pi / 180)
        let bx = (b.longitude - a.longitude) * metresPerDegreeLongitude
        let by = (b.latitude - a.latitude) * metresPerDegreeLatitude
        let px = (point.longitude - a.longitude) * metresPerDegreeLongitude
        let py = (point.latitude - a.latitude) * metresPerDegreeLatitude
        let lengthSquared = bx * bx + by * by
        let fraction = lengthSquared > 0 ? min(max((px * bx + py * by) / lengthSquared, 0), 1) : 0
        let dx = px - bx * fraction
        let dy = py - by * fraction
        return (fraction, (dx * dx + dy * dy).squareRoot())
    }
}
