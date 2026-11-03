import ActivityKit
import Foundation

final class NavigationLiveActivity: NavigationFeedback {
    private var activityID: String?
    private var lastState: NavigationActivityAttributes.ContentState?

    func start(destination: String, status: NavigationStatus) {
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        let state = Self.state(status)
        activityID = try? Activity.request(
            attributes: NavigationActivityAttributes(destination: destination),
            content: ActivityContent(state: state, staleDate: nil),
            pushType: nil
        ).id
        lastState = state
    }

    func update(_ status: NavigationStatus) {
        let state = Self.state(status)
        guard let activityID, state != lastState else { return }
        lastState = state
        Task { await Self.activity(activityID)?.update(ActivityContent(state: state, staleDate: nil)) }
    }

    func cue(_ cue: NavigationCue) {}

    func stop(_ status: NavigationStatus) {
        guard let activityID else { return }
        self.activityID = nil
        lastState = nil
        let content = ActivityContent(state: Self.state(status), staleDate: nil)
        let policy: ActivityUIDismissalPolicy = status.isFinished ? .after(.now.addingTimeInterval(15 * 60)) : .immediate
        Task { await Self.activity(activityID)?.end(content, dismissalPolicy: policy) }
    }

    private nonisolated static func activity(_ id: String) -> Activity<NavigationActivityAttributes>? {
        Activity<NavigationActivityAttributes>.activities.first { $0.id == id }
    }

    private static func state(_ status: NavigationStatus) -> NavigationActivityAttributes.ContentState {
        NavigationActivityAttributes.ContentState(
            title: status.title,
            detail: status.detail,
            symbol: status.symbol,
            arrival: status.arrival,
            isFinished: status.isFinished
        )
    }
}
