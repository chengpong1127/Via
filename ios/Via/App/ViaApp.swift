import SwiftUI

@main
struct ViaApp: App {
    @State private var session: JourneySession

    init() {
        _session = State(initialValue: JourneySession(
            planner: HTTPJourneyPlanner(baseURL: ViaAPIConfiguration.baseURL)
        ))
    }

    var body: some Scene {
        WindowGroup {
            JourneyMapView()
                .environment(session)
        }
    }
}
