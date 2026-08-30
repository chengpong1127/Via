import SwiftUI

@main
struct ViaApp: App {
    @State private var session = JourneySession(
        planner: DemoJourneyPlanner(),
        origin: SelectedPlace(
            displayText: "Taipei Main Station",
            coordinate: Coordinate(latitude: 25.0478, longitude: 121.5170)
        ),
        destination: SelectedPlace(
            displayText: "Taipei 101",
            coordinate: Coordinate(latitude: 25.0340, longitude: 121.5645)
        )
    )

    var body: some Scene {
        WindowGroup {
            JourneyMapView()
                .environment(session)
        }
    }
}
