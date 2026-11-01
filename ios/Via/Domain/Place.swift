import CoreLocation
import Foundation

nonisolated struct Coordinate: Hashable, Sendable {
    var latitude: Double
    var longitude: Double

    init(latitude: Double, longitude: Double) {
        self.latitude = latitude
        self.longitude = longitude
    }

    init(_ coordinate: CLLocationCoordinate2D) {
        self.init(latitude: coordinate.latitude, longitude: coordinate.longitude)
    }

    var clCoordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: latitude, longitude: longitude)
    }

    func distance(to other: Coordinate) -> CLLocationDistance {
        CLLocation(latitude: latitude, longitude: longitude)
            .distance(from: CLLocation(latitude: other.latitude, longitude: other.longitude))
    }
}

nonisolated struct Place: Hashable, Identifiable, Sendable {
    let id: String
    var name: String
    var subtitle: String?
    var coordinate: Coordinate

    init(id: String = UUID().uuidString, name: String, subtitle: String? = nil, coordinate: Coordinate) {
        self.id = id
        self.name = name
        self.subtitle = subtitle
        self.coordinate = coordinate
    }
}

nonisolated enum JourneyEndpoint: Hashable, Sendable {
    case currentLocation
    case place(Place)

    var place: Place? {
        if case let .place(place) = self { place } else { nil }
    }
}
