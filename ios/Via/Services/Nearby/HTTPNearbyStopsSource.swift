import Foundation

nonisolated struct HTTPNearbyStopsSource: NearbyStopsSource {
    private let client: APIClient

    init(client: APIClient) {
        self.client = client
    }

    func nearbyStops(around center: Coordinate, radiusMetres: Int) async throws -> NearbyStops {
        do {
            let dto = try await client.get(
                "v1/nearby-stops",
                query: [
                    URLQueryItem(name: "latitude", value: String(center.latitude)),
                    URLQueryItem(name: "longitude", value: String(center.longitude)),
                    URLQueryItem(name: "radius_metres", value: String(radiusMetres)),
                ],
                as: NearbyStopsDTO.self
            )
            return NearbyMapper.stops(from: dto)
        } catch let error as APIError {
            throw HTTPJourneyPlanner.planningError(for: error)
        }
    }
}
