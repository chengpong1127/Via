import CoreLocation
import Foundation

@MainActor
final class LocationService: NSObject, LocationProviding {
    private let manager: CLLocationManager
    private var authorizationContinuation: CheckedContinuation<Void, Error>?
    private var locationContinuation: CheckedContinuation<Coordinate, Error>?

    init(manager: CLLocationManager = CLLocationManager()) {
        self.manager = manager
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
    }

    var authorizationStatus: CLAuthorizationStatus {
        manager.authorizationStatus
    }

    func requestWhenInUseAuthorization() {
        manager.requestWhenInUseAuthorization()
    }

    func currentLocation() async throws -> Coordinate {
        switch authorizationStatus {
        case .notDetermined:
            requestWhenInUseAuthorization()
            if authorizationStatus != .notDetermined {
                return try await currentLocation()
            }
            try await waitForAuthorization()
            return try await currentLocation()
        case .authorizedWhenInUse, .authorizedAlways:
            return try await requestLocation()
        case .denied:
            throw LocationProvidingError.permissionDenied
        case .restricted:
            throw LocationProvidingError.restricted
        @unknown default:
            throw LocationProvidingError.unavailable
        }
    }

    private func waitForAuthorization() async throws {
        try await withCheckedThrowingContinuation { continuation in
            authorizationContinuation = continuation
        }
    }

    private func requestLocation() async throws -> Coordinate {
        try await withCheckedThrowingContinuation { continuation in
            locationContinuation = continuation
            manager.requestLocation()
        }
    }

    private func finishAuthorization(with result: Result<Void, Error>) {
        guard let continuation = authorizationContinuation else { return }
        authorizationContinuation = nil

        switch result {
        case .success:
            continuation.resume()
        case .failure(let error):
            continuation.resume(throwing: error)
        }
    }

    private func finishLocation(with result: Result<Coordinate, Error>) {
        guard let continuation = locationContinuation else { return }
        locationContinuation = nil

        switch result {
        case .success(let coordinate):
            continuation.resume(returning: coordinate)
        case .failure(let error):
            continuation.resume(throwing: error)
        }
    }
}

extension LocationService: CLLocationManagerDelegate {
    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        switch manager.authorizationStatus {
        case .authorizedWhenInUse, .authorizedAlways:
            finishAuthorization(with: .success(()))
        case .denied:
            finishAuthorization(with: .failure(LocationProvidingError.permissionDenied))
        case .restricted:
            finishAuthorization(with: .failure(LocationProvidingError.restricted))
        case .notDetermined:
            break
        @unknown default:
            finishAuthorization(with: .failure(LocationProvidingError.unavailable))
        }
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let location = locations.last else {
            finishLocation(with: .failure(LocationProvidingError.unavailable))
            return
        }

        finishLocation(with: .success(Coordinate(
            latitude: location.coordinate.latitude,
            longitude: location.coordinate.longitude
        )))
    }

    func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        if manager.authorizationStatus == .denied || (error as? CLError)?.code == .denied {
            finishLocation(with: .failure(LocationProvidingError.permissionDenied))
        } else {
            finishLocation(with: .failure(LocationProvidingError.unavailable))
        }
    }
}
