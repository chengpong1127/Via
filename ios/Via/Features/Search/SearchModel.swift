import MapKit
import Observation

@Observable
final class SearchModel {
    var query = "" {
        didSet {
            guard query != oldValue else { return }
            if results != nil { results = nil }
            if failed { failed = false }
            service.update(query: query)
        }
    }
    private(set) var results: [PlaceResult]?
    private(set) var isSearching = false
    private(set) var failed = false

    let service: PlaceSearchService
    private let nearbyStops: any NearbyStopsSource

    init(service: PlaceSearchService, nearbyStops: any NearbyStopsSource) {
        self.service = service
        self.nearbyStops = nearbyStops
    }

    var completions: [MKLocalSearchCompletion] { service.completions }

    func clear() {
        query = ""
        results = nil
    }

    func submit() async -> [PlaceResult] {
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return [] }
        return await load { try await service.results(for: text) }
    }

    func choose(_ completion: MKLocalSearchCompletion) async -> [PlaceResult] {
        await load { try await service.results(for: completion) }
    }

    func nearby(_ categories: [MKPointOfInterestCategory], around center: Coordinate) async -> [PlaceResult] {
        await load { try await service.nearby(categories, around: center, radius: 1_000) }
    }

    func nearbyTransit(_ mode: TransitMode, around center: Coordinate) async -> [PlaceResult] {
        await load {
            try await nearbyStops.nearbyStops(around: center, radiusMetres: 1_000).transitStops
                .filter { $0.modes.contains(mode) }
                .map(Self.result)
        }
    }

    func nearbyBikeStations(around center: Coordinate) async -> [PlaceResult] {
        await load {
            try await nearbyStops.nearbyStops(around: center, radiusMetres: 1_000).bikeStations
                .map(Self.result)
        }
    }

    private static func result(_ stop: NearbyTransitStop) -> PlaceResult {
        let labels = stop.routeLabels.prefix(6).joined(separator: " · ")
        let more = stop.routeLabels.count > 6 ? " +\(stop.routeLabels.count - 6)" : ""
        return result(
            id: stop.viaIDs.first ?? stop.name,
            name: stop.name,
            subtitle: labels.isEmpty ? nil : labels + more,
            coordinate: stop.coordinate,
            category: .publicTransport
        )
    }

    private static func result(_ station: NearbyBikeStation) -> PlaceResult {
        let subtitle = station.availability.map { availability in
            availability.inService
                ? [
                    String(localized: "\(availability.availableBikes) bikes"),
                    String(localized: "\(availability.availableDocks) docks"),
                ].joined(separator: " · ")
                : String(localized: "Not in service")
        }
        return result(id: station.viaID, name: station.name, subtitle: subtitle, coordinate: station.coordinate, category: nil)
    }

    private static func result(
        id: String,
        name: String,
        subtitle: String?,
        coordinate: Coordinate,
        category: MKPointOfInterestCategory?
    ) -> PlaceResult {
        let item = MKMapItem(
            location: CLLocation(latitude: coordinate.latitude, longitude: coordinate.longitude),
            address: nil
        )
        item.name = name
        item.pointOfInterestCategory = category
        return PlaceResult(
            mapItem: item,
            place: Place(id: id, name: name, subtitle: subtitle, coordinate: coordinate)
        )
    }

    private func load(_ operation: () async throws -> [PlaceResult]) async -> [PlaceResult] {
        isSearching = true
        failed = false
        defer { isSearching = false }
        do {
            let found = try await operation()
            results = found
            return found
        } catch {
            results = []
            failed = true
            return []
        }
    }
}
