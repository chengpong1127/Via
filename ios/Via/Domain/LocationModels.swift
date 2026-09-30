import CoreLocation
import Foundation

struct Coordinate: Hashable, Sendable {
    let latitude: Double
    let longitude: Double

    var clLocationCoordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }
}

struct Place: Hashable, Sendable {
    let name: String
    let subtitle: String?
    let coordinate: Coordinate

    init(name: String, subtitle: String? = nil, coordinate: Coordinate) {
        self.name = name
        self.subtitle = subtitle
        self.coordinate = coordinate
    }

    var displayText: String { name }
}

enum JourneyEndpoint: Hashable, Sendable {
    case currentLocation
    case place(Place)
}

enum LocationProvidingError: Error, Equatable, Sendable {
    case permissionDenied
    case restricted
    case unavailable
}

@MainActor
protocol LocationProviding {
    var authorizationStatus: CLAuthorizationStatus { get }

    func requestWhenInUseAuthorization()
    func currentLocation() async throws -> Coordinate
}
