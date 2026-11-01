import MapKit
import Observation
import SwiftUI

struct PlaceResult: Identifiable, Hashable {
    let place: Place
    let mapItem: MKMapItem

    var id: String { place.id }

    init(mapItem: MKMapItem) {
        self.mapItem = mapItem
        place = Place(
            id: mapItem.identifier?.rawValue ?? UUID().uuidString,
            name: mapItem.name ?? mapItem.address?.shortAddress ?? String(localized: "Dropped Pin"),
            subtitle: mapItem.address?.shortAddress ?? mapItem.addressRepresentations?.cityWithContext,
            coordinate: Coordinate(mapItem.location.coordinate)
        )
    }
}

@Observable
final class PlaceSearchService: NSObject {
    private(set) var completions: [MKLocalSearchCompletion] = []
    private(set) var completionFailed = false

    @ObservationIgnored private let completer = MKLocalSearchCompleter()
    @ObservationIgnored var region = MKCoordinateRegion(
        center: CLLocationCoordinate2D(latitude: 25.0418, longitude: 121.5434),
        span: MKCoordinateSpan(latitudeDelta: 0.3, longitudeDelta: 0.3)
    ) {
        didSet { completer.region = region }
    }

    override init() {
        super.init()
        completer.delegate = self
        completer.resultTypes = [.address, .pointOfInterest, .query]
        completer.region = region
    }

    func update(query: String) {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty {
            completer.cancel()
            completions = []
            completionFailed = false
        } else {
            completer.queryFragment = trimmed
        }
    }

    func results(for completion: MKLocalSearchCompletion) async throws -> [PlaceResult] {
        try await run(MKLocalSearch.Request(completion: completion))
    }

    func results(for query: String) async throws -> [PlaceResult] {
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = query
        return try await run(request)
    }

    func result(for feature: MapFeature) async throws -> PlaceResult {
        PlaceResult(mapItem: try await MKMapItemRequest(feature: feature).mapItem)
    }

    func result(at coordinate: Coordinate) async -> PlaceResult {
        let location = CLLocation(latitude: coordinate.latitude, longitude: coordinate.longitude)
        if let request = MKReverseGeocodingRequest(location: location),
           let item = try? await request.mapItems.first {
            let result = PlaceResult(mapItem: item)
            return PlaceResult(
                mapItem: item,
                place: Place(name: result.place.name, subtitle: result.place.subtitle, coordinate: coordinate)
            )
        }
        let item = MKMapItem(location: location, address: nil)
        return PlaceResult(mapItem: item, place: Place(name: String(localized: "Dropped Pin"), coordinate: coordinate))
    }

    private func run(_ request: MKLocalSearch.Request) async throws -> [PlaceResult] {
        request.region = region
        request.resultTypes = [.pointOfInterest, .address]
        let response = try await MKLocalSearch(request: request).start()
        return response.mapItems.map(PlaceResult.init)
    }
}

extension PlaceResult {
    init(mapItem: MKMapItem, place: Place) {
        self.mapItem = mapItem
        self.place = place
    }
}

extension PlaceSearchService: MKLocalSearchCompleterDelegate {
    nonisolated func completerDidUpdateResults(_ completer: MKLocalSearchCompleter) {
        MainActor.assumeIsolated {
            completions = self.completer.results
            completionFailed = false
        }
    }

    nonisolated func completer(_ completer: MKLocalSearchCompleter, didFailWithError error: Error) {
        MainActor.assumeIsolated {
            completions = []
            completionFailed = true
        }
    }
}
