import ActivityKit
import Foundation

nonisolated struct NavigationActivityAttributes: ActivityAttributes {
    nonisolated struct ContentState: Codable, Hashable, Sendable {
        var title: String
        var detail: String
        var symbol: String
        var arrival: Date
        var isFinished: Bool
    }

    var destination: String
}
