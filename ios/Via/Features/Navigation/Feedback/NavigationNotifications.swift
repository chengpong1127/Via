import UIKit
import UserNotifications

final class NavigationNotifications: NavigationFeedback {
    private let center = UNUserNotificationCenter.current()

    func start(destination: String, status: NavigationStatus) {
        Task { _ = try? await center.requestAuthorization(options: [.alert, .sound]) }
    }

    func update(_ status: NavigationStatus) {}

    func cue(_ cue: NavigationCue) {
        guard Self.notifies(cue), UIApplication.shared.applicationState != .active else { return }
        let content = UNMutableNotificationContent()
        content.title = Self.title(for: cue)
        content.body = cue.message
        content.sound = .default
        content.interruptionLevel = .active
        content.threadIdentifier = "navigation"
        center.add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    func stop(_ status: NavigationStatus) {
        center.removeAllDeliveredNotifications()
    }

    private static func notifies(_ cue: NavigationCue) -> Bool {
        switch cue {
        case .maneuver, .alert(.stopsLeft): false
        case .alert, .rerouted, .rerouteFailed: true
        }
    }

    private static func title(for cue: NavigationCue) -> String {
        switch cue {
        case .alert(.leaveNow): String(localized: "Time to Leave")
        case .alert(.hurry): String(localized: "Running Late")
        case .alert(.nextStop), .alert(.getOff): String(localized: "Your Stop")
        case .alert(.arrived): String(localized: "Arrived")
        case .rerouted: String(localized: "Route Updated")
        case .rerouteFailed: String(localized: "Route Not Updated")
        case .maneuver, .alert(.stopsLeft): String(localized: "Via")
        }
    }
}
