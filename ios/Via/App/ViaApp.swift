import SwiftUI

@main
struct ViaApp: App {
    @State private var model: ExploreModel

    init() {
        let location = LocationService()
        let client = APIClient(baseURL: ViaServer.baseURL)
        let planner = HTTPJourneyPlanner(client: client)
        _model = State(initialValue: ExploreModel(
            search: SearchModel(service: PlaceSearchService(), nearbyStops: HTTPNearbyStopsSource(client: client)),
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
