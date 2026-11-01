import SwiftUI

@main
struct ViaApp: App {
    @State private var model: ExploreModel

    init() {
        let location = LocationService()
        let planner = HTTPJourneyPlanner(client: APIClient(baseURL: ViaServer.baseURL))
        _model = State(initialValue: ExploreModel(
            search: SearchModel(service: PlaceSearchService()),
            directions: DirectionsModel(planner: planner, location: location),
            location: location
        ))
    }

    var body: some Scene {
        WindowGroup {
            ExploreView(model: model)
        }
    }
}
