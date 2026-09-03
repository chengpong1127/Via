import SwiftUI

@main
struct ViaApp: App {
    @State private var session = JourneySession(
        planner: DemoJourneyPlanner()
    )

    var body: some Scene {
        WindowGroup {
            JourneyMapView()
                .environment(session)
        }
    }
}
