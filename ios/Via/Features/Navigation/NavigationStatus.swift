import Foundation

nonisolated struct NavigationStatus: Hashable, Sendable {
    let title: String
    let detail: String
    let symbol: String
    let arrival: Date
    let isFinished: Bool
}

nonisolated enum NavigationCue: Hashable, Sendable {
    case maneuver(String)
    case alert(NavigationAlert)
    case rerouted
    case rerouteFailed

    var message: String {
        switch self {
        case let .maneuver(instruction): instruction
        case let .alert(alert): alert.message
        case .rerouted: String(localized: "Route updated")
        case .rerouteFailed: String(localized: "Couldn't update the route. Keep following the current one.")
        }
    }
}

protocol NavigationFeedback: AnyObject {
    func start(destination: String, status: NavigationStatus)
    func update(_ status: NavigationStatus)
    func cue(_ cue: NavigationCue)
    func stop(_ status: NavigationStatus)
}

extension NavigationAlert {
    nonisolated var message: String {
        switch self {
        case .leaveNow: String(localized: "Time to leave")
        case .hurry: String(localized: "Hurry to make your connection")
        case let .stopsLeft(count): String(localized: "\(count) stops left")
        case let .nextStop(name): String(localized: "Next stop: \(name)")
        case let .getOff(name): String(localized: "Get off at \(name)")
        case .arrived: String(localized: "You've arrived")
        }
    }
}

extension Guidance {
    nonisolated var instruction: String {
        let action = switch code {
        case "continue": String(localized: "Continue straight")
        case "turn_left": String(localized: "Turn left")
        case "turn_right": String(localized: "Turn right")
        case "u_turn": String(localized: "Make a U-turn")
        case "enter_roundabout": String(localized: "Enter the roundabout")
        case "exit_roundabout": String(localized: "Exit the roundabout")
        case "arrive": String(localized: "You're arriving")
        default: String(localized: "Continue")
        }
        guard let streetName, code != "arrive" else { return action }
        return String(localized: "\(action) onto \(streetName)")
    }

    nonisolated var symbol: String {
        switch code {
        case "turn_left": "arrow.turn.up.left"
        case "turn_right": "arrow.turn.up.right"
        case "u_turn": "arrow.uturn.down"
        case "enter_roundabout", "exit_roundabout": "arrow.triangle.2.circlepath"
        case "arrive": "mappin.and.ellipse"
        default: "arrow.up"
        }
    }
}

extension NavigationModel {
    var status: NavigationStatus {
        let leg = currentLeg.leg
        func status(_ title: String, _ detail: String, _ symbol: String) -> NavigationStatus {
            NavigationStatus(title: title, detail: detail, symbol: symbol, arrival: estimatedArrival, isFinished: isFinished)
        }
        if isFinished {
            return status(String(localized: "You've arrived"), destination.name, "mappin.circle.fill")
        }
        if case let .transit(ride) = leg.kind {
            guard tracker.boarded else {
                let time = leg.departureTime.formatted(.clockTime)
                return status(
                    String(localized: "Board \(ride.serviceLabel) at \(leg.from.name)"),
                    ride.headsign.isEmpty
                        ? String(localized: "Departs \(time)")
                        : String(localized: "Toward \(ride.headsign) · \(time)"),
                    ride.mode.symbol
                )
            }
            let stops = tracker.stopsRemaining ?? 0
            return status(
                stops <= 1
                    ? String(localized: "Get off at \(leg.to.name)")
                    : String(localized: "\(stops) stops to \(leg.to.name)"),
                String(localized: "\(ride.serviceLabel) · arrives \(leg.arrivalTime.formatted(.clockTime))"),
                ride.mode.symbol
            )
        }
        if let maneuver = tracker.nextManeuver, maneuver.guidance.code != "arrive",
           let distance = tracker.distanceToManeuver {
            return status(maneuver.guidance.instruction, distance.navigationDistance, maneuver.guidance.symbol)
        }
        if case let .bikeShare(ride) = leg.kind {
            return status(
                String(localized: "Return the bike at \(ride.dropoffStationName)"),
                String(localized: "Return by \(leg.arrivalTime.formatted(.clockTime))"),
                "bicycle"
            )
        }
        return status(
            String(localized: "Walk to \(leg.to.name)"),
            tracker.remainingLegDistance.navigationDistance,
            "figure.walk"
        )
    }
}
