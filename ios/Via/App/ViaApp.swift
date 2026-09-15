import SwiftUI

@MainActor
@main
struct ViaApp: App {
    @State private var session: JourneySession

    init() {
        let locationService = LocationService()
        _session = State(initialValue: JourneySession(
            planner: HTTPJourneyPlanner(baseURL: ViaAPIConfiguration.baseURL),
            locationProvider: locationService
        ))
    }

    var body: some Scene {
        WindowGroup {
            JourneyMapView()
                .environment(session)
        }
    }
}
