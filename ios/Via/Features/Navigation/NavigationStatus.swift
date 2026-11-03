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
        case let .nextStop(name): String(localized: "Next stop: \(name). Get ready to get off.")
        case let .getOff(name): String(localized: "Get off at \(name)")
        case .arrived: String(localized: "You've arrived")
        }
    }
}

extension Guidance {
    nonisolated var spokenInstruction: String {
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
}

extension NavigationModel {
    var status: NavigationStatus {
        let leg = currentLeg.leg
        let arrival = estimatedArrival
        if isFinished {
            return NavigationStatus(
                title: String(localized: "You've arrived"),
                detail: destination.name,
                symbol: "mappin.circle.fill",
                arrival: arrival,
                isFinished: true
            )
        }
        switch leg.kind {
        case let .transit(ride):
            if tracker.boarded {
                let stops = tracker.stopsRemaining ?? 0
                return NavigationStatus(
                    title: stops <= 1
                        ? String(localized: "Get off at \(leg.to.name)")
                        : String(localized: "\(stops) stops to \(leg.to.name)"),
                    detail: String(localized: "\(ride.serviceLabel) · arrives \(leg.arrivalTime.formatted(.clockTime))"),
                    symbol: ride.mode.symbol,
                    arrival: arrival,
                    isFinished: false
                )
            }
            return NavigationStatus(
                title: String(localized: "Board \(ride.serviceLabel) at \(leg.from.name)"),
                detail: String(localized: "Departs \(leg.departureTime.formatted(.clockTime))"),
                symbol: ride.mode.symbol,
                arrival: arrival,
                isFinished: false
            )
        case let .bikeShare(ride):
            if let maneuver = tracker.nextManeuver, maneuver.guidance.code != "arrive",
               let distance = tracker.distanceToManeuver {
                return NavigationStatus(
                    title: maneuver.guidance.spokenInstruction,
                    detail: distance.navigationDistance,
                    symbol: maneuver.guidance.navigationSymbol,
                    arrival: arrival,
                    isFinished: false
                )
            }
            return NavigationStatus(
                title: String(localized: "Return the bike at \(ride.dropoffStationName)"),
                detail: String(localized: "Return by \(leg.arrivalTime.formatted(.clockTime))"),
                symbol: "bicycle",
                arrival: arrival,
                isFinished: false
            )
        case .walking:
            if let maneuver = tracker.nextManeuver, maneuver.guidance.code != "arrive",
               let distance = tracker.distanceToManeuver {
                return NavigationStatus(
                    title: maneuver.guidance.spokenInstruction,
                    detail: distance.navigationDistance,
                    symbol: maneuver.guidance.navigationSymbol,
                    arrival: arrival,
                    isFinished: false
                )
            }
            return NavigationStatus(
                title: String(localized: "Walk to \(leg.to.name)"),
                detail: tracker.remainingLegDistance.navigationDistance,
                symbol: "figure.walk",
                arrival: arrival,
                isFinished: false
            )
        }
    }
}
