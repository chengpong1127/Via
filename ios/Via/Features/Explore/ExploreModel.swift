import MapKit
import Observation
import SwiftUI

enum EndpointRole: Hashable {
    case origin
    case destination
}

enum SheetRoute: Hashable {
    case search
    case results
    case place(PlaceResult)
    case directions
    case journey(String)
    case pickEndpoint(EndpointRole)
}

extension PresentationDetent {
    static let collapsed = PresentationDetent.height(92)
}

extension MKCoordinateRegion {
    static let taipei = MKCoordinateRegion(
        center: CLLocationCoordinate2D(latitude: 25.0418, longitude: 121.5434),
        span: MKCoordinateSpan(latitudeDelta: 0.12, longitudeDelta: 0.12)
    )
}

@Observable
final class ExploreModel {
    var route: SheetRoute = .search
    var detent: PresentationDetent = .collapsed
    var camera: MapCameraPosition = .userLocation(fallback: .region(.taipei))
    var mapSelection: MapSelection<String>?
    @ObservationIgnored var mapSize = CGSize(width: 402, height: 874)
    private(set) var selectedPlace: PlaceResult?
    private(set) var focusedLegID: String?

    let search: SearchModel
    let directions: DirectionsModel
    let location: LocationService

    init(search: SearchModel, directions: DirectionsModel, location: LocationService) {
        self.search = search
        self.directions = directions
        self.location = location
    }

    var isPlanning: Bool {
        switch route {
        case .directions, .journey, .pickEndpoint: true
        default: false
        }
    }

    var resultMarkers: [PlaceResult] {
        route == .results || selectedPlace != nil && !isPlanning ? search.results ?? [] : []
    }

    var journeyOption: JourneyOption? {
        guard case let .journey(id) = route else { return nil }
        return directions.options.first { $0.id == id }
    }

    func showResults(_ results: [PlaceResult]) {
        if results.count == 1, let only = results.first {
            show(only)
            return
        }
        route = .results
        detent = .medium
        fit(results.map(\.place.coordinate))
    }

    func show(_ result: PlaceResult) {
        selectedPlace = result
        route = .place(result)
        detent = .medium
        fit([result.place.coordinate])
    }

    func closePlace() {
        selectedPlace = nil
        mapSelection = nil
        if search.results?.isEmpty == false {
            route = .results
        } else {
            search.clear()
            route = .search
            detent = .collapsed
        }
    }

    func closeResults() {
        search.clear()
        route = .search
        detent = .collapsed
    }

    func startDirections(to result: PlaceResult) {
        directions.reset()
        directions.origin = .currentLocation
        directions.destination = .place(result.place)
        route = .directions
        detent = .medium
        replan()
    }

    func closeDirections() {
        directions.reset()
        focusedLegID = nil
        if let selectedPlace {
            route = .place(selectedPlace)
            fit([selectedPlace.place.coordinate])
        } else {
            route = .search
            detent = .collapsed
        }
    }

    func pickEndpoint(_ role: EndpointRole) {
        search.clear()
        route = .pickEndpoint(role)
        detent = .large
    }

    func choose(_ endpoint: JourneyEndpoint, for role: EndpointRole) {
        switch role {
        case .origin: directions.origin = endpoint
        case .destination: directions.destination = endpoint
        }
        search.clear()
        route = .directions
        detent = .medium
        directions.reset()
        replan()
    }

    func cancelEndpointPicking() {
        search.clear()
        route = .directions
        detent = .medium
    }

    func swapEndpoints() {
        directions.swapEndpoints()
        directions.reset()
        replan()
    }

    func replan() {
        Task {
            await directions.plan()
            if let option = directions.selectedOption {
                fit(option.overview)
            }
        }
    }

    func select(_ option: JourneyOption) {
        directions.selectedOptionID = option.id
        fit(option.overview)
    }

    func showJourney(_ option: JourneyOption) {
        directions.selectedOptionID = option.id
        focusedLegID = nil
        route = .journey(option.id)
        fit(option.overview)
    }

    func closeJourney() {
        focusedLegID = nil
        route = .directions
        if let option = directions.selectedOption {
            fit(option.overview)
        }
    }

    func focus(_ leg: Leg) {
        focusedLegID = leg.id
        fit(leg.path + [leg.from.coordinate, leg.to.coordinate])
    }

    func handleMapSelection() async {
        guard let selection = mapSelection, !isPlanning else { return }
        if let id = selection.value, let result = resultMarkers.first(where: { $0.id == id }) {
            show(result)
        } else if let feature = selection.feature,
                  let result = try? await search.service.result(for: feature) {
            show(result)
        }
    }

    func dropPin(at coordinate: Coordinate) async {
        guard !isPlanning else { return }
        show(await search.service.result(at: coordinate))
    }

    func fit(_ coordinates: [Coordinate]) {
        guard let first = coordinates.first else { return }
        var rect = MKMapRect(origin: MKMapPoint(first.clCoordinate), size: MKMapSize(width: 0, height: 0))
        for coordinate in coordinates.dropFirst() {
            rect = rect.union(MKMapRect(origin: MKMapPoint(coordinate.clCoordinate), size: MKMapSize(width: 0, height: 0)))
        }
        let topInset = 70.0
        let visibleHeight = max(mapSize.height * 0.5 - topInset, 120)
        let minimum = 1_000 * MKMapPointsPerMeterAtLatitude(first.latitude)
        let scale = max(
            rect.width / (mapSize.width * 0.8),
            rect.height / (visibleHeight * 0.8),
            minimum / mapSize.width
        )
        let padded = MKMapRect(
            x: rect.midX - mapSize.width * scale / 2,
            y: rect.midY - (topInset + visibleHeight / 2) * scale,
            width: mapSize.width * scale,
            height: mapSize.height * scale
        )
        withAnimation(.smooth) {
            camera = .rect(padded)
        }
    }
}
