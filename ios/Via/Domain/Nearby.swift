import Foundation

nonisolated struct NearbyStops: Sendable {
    let transitStops: [NearbyTransitStop]
    let bikeStations: [NearbyBikeStation]
}

nonisolated struct NearbyTransitStop: Hashable, Sendable {
    let viaIDs: [String]
    let name: String
    let coordinate: Coordinate
    let distance: Double
    let modes: [TransitMode]
    let routeLabels: [String]
}

nonisolated struct NearbyBikeStation: Hashable, Sendable {
    let viaID: String
    let name: String
    let coordinate: Coordinate
    let distance: Double
    let availability: BikeAvailability?
}

nonisolated struct BikeAvailability: Hashable, Sendable {
    let availableBikes: Int
    let availableDocks: Int
    let inService: Bool
}

nonisolated protocol NearbyStopsSource: Sendable {
    func nearbyStops(around center: Coordinate, radiusMetres: Int) async throws -> NearbyStops
}
