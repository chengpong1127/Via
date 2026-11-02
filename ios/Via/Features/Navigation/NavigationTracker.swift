import Foundation

nonisolated enum NavigationAlert: Hashable, Sendable {
    case leaveNow
    case hurry
    case stopsLeft(Int)
    case nextStop(String)
    case getOff(String)
    case arrived
}

nonisolated enum RerouteReason: Hashable, Sendable {
    case offRoute
    case missedConnection
}

nonisolated struct NavigationUpdate: Equatable, Sendable {
    var alerts: [NavigationAlert] = []
    var reroute: RerouteReason?
    var legChanged = false

    mutating func merge(_ other: NavigationUpdate) {
        alerts += other.alerts
        reroute = reroute ?? other.reroute
        legChanged = legChanged || other.legChanged
    }
}

nonisolated struct NavigationTracker: Sendable {
    static let arrivalRadius = 20.0
    static let offRouteDistance = 40.0
    static let offRouteFixCount = 3
    static let lookahead = 300.0
    static let boardingRadius = 150.0
    static let exitRadius = 150.0
    static let stopPassMargin = 50.0
    static let missedGrace: TimeInterval = 120
    static let alightLead: TimeInterval = 120
    static let staleFix: TimeInterval = 60
    static let walkingSpeed = 1.3
    static let usableAccuracy = 100.0

    let route: NavigationRoute
    let startedAt: Date
    private(set) var legIndex = 0
    private(set) var distanceAlong = 0.0
    private(set) var boarded = false
    private(set) var isFinished = false
    private(set) var lastFix: LocationFix?
    private var offRouteFixes = 0
    private var raised: Set<RaisedAlert> = []

    private struct RaisedAlert: Hashable {
        let leg: Int
        let alert: NavigationAlert
    }

    init(route: NavigationRoute, startedAt: Date) {
        self.route = route
        self.startedAt = startedAt
        isFinished = route.legs.isEmpty
    }

    var currentLeg: NavigationLeg { route.legs[min(legIndex, route.legs.count - 1)] }

    var nextLeg: NavigationLeg? {
        legIndex + 1 < route.legs.count ? route.legs[legIndex + 1] : nil
    }

    var nextManeuver: NavigationManeuver? {
        guard currentLeg.isStreet else { return nil }
        return currentLeg.maneuvers.first { $0.distanceAlong > distanceAlong + 5 } ?? currentLeg.maneuvers.last
    }

    var distanceToManeuver: Double? {
        nextManeuver.map { max($0.distanceAlong - distanceAlong, 0) }
    }

    var remainingLegDistance: Double {
        max(currentLeg.length - distanceAlong, 0)
    }

    var stopsRemaining: Int? {
        guard !currentLeg.isStreet else { return nil }
        return currentLeg.stopDistances.dropFirst().filter { $0 > distanceAlong + Self.stopPassMargin }.count
    }

    func estimatedArrival(now: Date) -> Date {
        let later = route.legs.dropFirst(legIndex + 1)
        guard currentLeg.isStreet, later.allSatisfy(\.isStreet) else { return route.option.arrivalTime }
        let fraction = currentLeg.length > 0 ? remainingLegDistance / currentLeg.length : 0
        return now.addingTimeInterval(currentLeg.leg.duration * fraction + later.map(\.leg.duration).reduce(0, +))
    }

    mutating func update(_ fix: LocationFix) -> NavigationUpdate {
        var update = NavigationUpdate()
        guard !isFinished else { return update }
        lastFix = fix
        if currentLeg.isStreet {
            trackStreet(fix, &update)
        } else {
            trackTransit(fix, &update)
        }
        update.merge(tick(now: fix.timestamp))
        return update
    }

    mutating func tick(now: Date) -> NavigationUpdate {
        var update = NavigationUpdate()
        guard !isFinished else { return update }
        if currentLeg.isStreet {
            checkDeparture(now: now, &update)
        } else {
            checkTransitTiming(now: now, &update)
        }
        return update
    }

    private mutating func trackStreet(_ fix: LocationFix, _ update: inout NavigationUpdate) {
        guard fix.horizontalAccuracy >= 0, fix.horizontalAccuracy <= Self.usableAccuracy,
              let projection = currentLeg.project(
                  fix.coordinate,
                  from: distanceAlong - 30,
                  through: distanceAlong + Self.lookahead
              )
        else { return }
        if projection.offset > max(Self.offRouteDistance, fix.horizontalAccuracy) {
            offRouteFixes += 1
            if offRouteFixes >= Self.offRouteFixCount { update.reroute = .offRoute }
            return
        }
        offRouteFixes = 0
        distanceAlong = max(distanceAlong, projection.distanceAlong)
        if currentLeg.length - distanceAlong <= Self.arrivalRadius {
            advance(&update)
        }
    }

    private mutating func trackTransit(_ fix: LocationFix, _ update: inout NavigationUpdate) {
        guard fix.horizontalAccuracy >= 0, fix.horizontalAccuracy <= Self.usableAccuracy * 2 else { return }
        let leg = currentLeg
        let nearExit = fix.coordinate.distance(to: leg.leg.to.coordinate) <= Self.exitRadius
        let onNextLeg = nextLeg.flatMap { $0.isStreet ? $0.project(fix.coordinate, through: Self.lookahead) : nil }
            .map { $0.offset <= Self.offRouteDistance && $0.distanceAlong > Self.arrivalRadius } ?? false
        let alightingWindow = fix.timestamp >= leg.leg.arrivalTime.addingTimeInterval(-Self.alightLead)
        if (boarded || alightingWindow) && (nearExit || onNextLeg) && leg.length > Self.exitRadius {
            raise(.getOff(leg.leg.to.name), &update)
            advance(&update)
            return
        }
        guard let projection = leg.project(fix.coordinate), projection.offset <= Self.boardingRadius * 2 else { return }
        if !boarded {
            let fromBoarding = fix.coordinate.distance(to: leg.leg.from.coordinate)
            guard fromBoarding > Self.boardingRadius, projection.distanceAlong > Self.boardingRadius else { return }
            boarded = true
            update.legChanged = true
        }
        distanceAlong = max(distanceAlong, projection.distanceAlong)
        announceStops(&update)
    }

    private mutating func announceStops(_ update: inout NavigationUpdate) {
        guard let remaining = stopsRemaining else { return }
        let leg = currentLeg.leg
        switch remaining {
        case 0:
            raise(.getOff(leg.to.name), &update)
            advance(&update)
        case 1:
            raise(.nextStop(leg.to.name), &update)
        case 2:
            raise(.stopsLeft(2), &update)
        default:
            break
        }
    }

    private mutating func checkDeparture(now: Date, _ update: inout NavigationUpdate) {
        let leg = currentLeg.leg
        if legIndex == 0, distanceAlong < Self.arrivalRadius, now >= leg.departureTime,
           startedAt < leg.departureTime.addingTimeInterval(-30) {
            raise(.leaveNow, &update)
        }
        guard let next = nextLeg, !next.isStreet else { return }
        let available = next.leg.departureTime.timeIntervalSince(now)
        let needed = remainingLegDistance / Self.walkingSpeed
        if available < -Self.missedGrace {
            update.reroute = .missedConnection
        } else if lastFix != nil, available > 0, needed > available {
            raise(.hurry, &update)
        }
    }

    private mutating func checkTransitTiming(now: Date, _ update: inout NavigationUpdate) {
        let leg = currentLeg.leg
        let stale = lastFix.map { now.timeIntervalSince($0.timestamp) > Self.staleFix } ?? true
        if !boarded, now > leg.departureTime.addingTimeInterval(Self.missedGrace) {
            if stale {
                boarded = true
                update.legChanged = true
            } else if let fix = lastFix, fix.coordinate.distance(to: leg.from.coordinate) <= Self.boardingRadius {
                update.reroute = .missedConnection
            }
        }
        if stale, now >= leg.arrivalTime.addingTimeInterval(-Self.alightLead) {
            raise(.nextStop(leg.to.name), &update)
        }
    }

    private mutating func raise(_ alert: NavigationAlert, _ update: inout NavigationUpdate) {
        if raised.insert(RaisedAlert(leg: legIndex, alert: alert)).inserted {
            update.alerts.append(alert)
        }
    }

    private mutating func advance(_ update: inout NavigationUpdate) {
        update.legChanged = true
        offRouteFixes = 0
        boarded = false
        distanceAlong = 0
        if legIndex + 1 < route.legs.count {
            legIndex += 1
        } else {
            isFinished = true
            raise(.arrived, &update)
        }
    }
}
