import Foundation

nonisolated struct NearbyStopsDTO: Decodable, Sendable {
    let transitStops: [NearbyTransitStopDTO]
    let bikeStations: [NearbyBikeStationDTO]

    enum CodingKeys: String, CodingKey {
        case transitStops = "transit_stops"
        case bikeStations = "bike_stations"
    }
}

nonisolated struct NearbyTransitStopDTO: Decodable, Sendable {
    let viaIDs: [String]
    let name: String
    let coordinate: CoordinateDTO
    let distanceMetres: Int
    let modes: [String]
    let routeLabels: [String]

    enum CodingKeys: String, CodingKey {
        case viaIDs = "via_ids"
        case name
        case coordinate
        case distanceMetres = "distance_metres"
        case modes
        case routeLabels = "route_labels"
    }
}

nonisolated struct NearbyBikeStationDTO: Decodable, Sendable {
    let viaID: String
    let name: String
    let coordinate: CoordinateDTO
    let distanceMetres: Int
    let availability: BikeAvailabilityDTO?

    enum CodingKeys: String, CodingKey {
        case viaID = "via_id"
        case name
        case coordinate
        case distanceMetres = "distance_metres"
        case availability
    }
}

nonisolated struct BikeAvailabilityDTO: Decodable, Sendable {
    let availableBikes: Int
    let availableReturnSlots: Int
    let inService: Bool

    enum CodingKeys: String, CodingKey {
        case availableBikes = "available_bikes"
        case availableReturnSlots = "available_return_slots"
        case inService = "in_service"
    }
}

nonisolated enum NearbyMapper {
    static func stops(from dto: NearbyStopsDTO) -> NearbyStops {
        NearbyStops(
            transitStops: dto.transitStops.map { stop in
                NearbyTransitStop(
                    viaIDs: stop.viaIDs,
                    name: stop.name,
                    coordinate: Coordinate(latitude: stop.coordinate.latitude, longitude: stop.coordinate.longitude),
                    distance: Double(stop.distanceMetres),
                    modes: stop.modes.compactMap(TransitMode.init(rawValue:)),
                    routeLabels: stop.routeLabels
                )
            },
            bikeStations: dto.bikeStations.map { station in
                NearbyBikeStation(
                    viaID: station.viaID,
                    name: station.name,
                    coordinate: Coordinate(latitude: station.coordinate.latitude, longitude: station.coordinate.longitude),
                    distance: Double(station.distanceMetres),
                    availability: station.availability.map {
                        BikeAvailability(
                            availableBikes: $0.availableBikes,
                            availableDocks: $0.availableReturnSlots,
                            inService: $0.inService
                        )
                    }
                )
            }
        )
    }
}
