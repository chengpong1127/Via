import CoreLocation
import MapKit
import Testing
@testable import Via

struct LocationSearchTests {
    @Test("The current map viewport is the first search-region preference")
    func mapViewportWinsOverNearbyContext() {
        let viewport = MKCoordinateRegion(
            center: CLLocationCoordinate2D(latitude: 22.6273, longitude: 120.3014),
            span: MKCoordinateSpan(latitudeDelta: 0.15, longitudeDelta: 0.15)
        )

        let region = LocationSearchRegion.preferred(
            mapViewport: viewport,
            userCoordinate: Coordinate(latitude: 25.04, longitude: 121.54)
        )

        #expect(region.center.latitude == viewport.center.latitude)
        #expect(region.center.longitude == viewport.center.longitude)
        #expect(region.span.latitudeDelta == viewport.span.latitudeDelta)
    }

    @Test("An authorized user location is the second search-region preference")
    func userLocationIsUsedWhenTheMapViewportIsUnavailable() {
        let region = LocationSearchRegion.preferred(
            mapViewport: nil,
            userCoordinate: Coordinate(latitude: 24.1477, longitude: 120.6736)
        )

        #expect(region.center.latitude == 24.1477)
        #expect(region.center.longitude == 120.6736)
        #expect(region.span.latitudeDelta == 0.5)
    }

    @Test("Search fallback is Taiwan-wide rather than Taipei-only")
    func fallbackRegionIsTaiwanWide() {
        let region = LocationSearchRegion.preferred(mapViewport: nil, userCoordinate: nil)

        #expect(region.center.latitude == 23.7)
        #expect(region.center.longitude == 120.96)
        #expect(region.span.latitudeDelta > 4)
        #expect(region.span.longitudeDelta > 4)
    }
}
