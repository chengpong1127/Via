import CoreLocation
import Observation

enum LocationError: Error, Equatable {
    case denied
    case restricted
    case unavailable
}

nonisolated struct LocationFix: Hashable, Sendable {
    let coordinate: Coordinate
    let horizontalAccuracy: Double
    let timestamp: Date
}

protocol LocationTracking: AnyObject {
    func fixes() -> AsyncStream<LocationFix>
    func beginNavigation()
    func endNavigation()
}

protocol LocationProviding: AnyObject {
    func currentCoordinate() async throws -> Coordinate
}

@Observable
final class LocationService: NSObject, LocationProviding, LocationTracking {
    private(set) var authorizationStatus: CLAuthorizationStatus
    private(set) var coordinate: Coordinate?
    private(set) var heading: Double?

    @ObservationIgnored private let manager = CLLocationManager()
    @ObservationIgnored private var waiters: [CheckedContinuation<Coordinate, Error>] = []
    @ObservationIgnored private var subscribers: [UUID: AsyncStream<LocationFix>.Continuation] = [:]
    @ObservationIgnored private var backgroundSession: CLBackgroundActivitySession?

    override init() {
        authorizationStatus = manager.authorizationStatus
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyNearestTenMeters
        manager.headingFilter = 2
        if isAuthorized { startUpdates() }
    }

    private func startUpdates() {
        manager.startUpdatingLocation()
        if CLLocationManager.headingAvailable() { manager.startUpdatingHeading() }
    }

    var isAuthorized: Bool {
        authorizationStatus == .authorizedWhenInUse || authorizationStatus == .authorizedAlways
    }

    func requestAuthorizationIfNeeded() {
        if authorizationStatus == .notDetermined {
            manager.requestWhenInUseAuthorization()
        }
    }

    func currentCoordinate() async throws -> Coordinate {
        switch authorizationStatus {
        case .denied: throw LocationError.denied
        case .restricted: throw LocationError.restricted
        default: break
        }
        if let coordinate, isAuthorized {
            return coordinate
        }
        requestAuthorizationIfNeeded()
        let timeout = Task {
            repeat {
                try await Task.sleep(for: .seconds(15))
            } while authorizationStatus == .notDetermined
            resumeWaiters(with: .failure(LocationError.unavailable))
        }
        defer { timeout.cancel() }
        return try await withCheckedThrowingContinuation { continuation in
            waiters.append(continuation)
            if isAuthorized { manager.startUpdatingLocation() }
        }
    }

    func fixes() -> AsyncStream<LocationFix> {
        let (stream, continuation) = AsyncStream.makeStream(of: LocationFix.self, bufferingPolicy: .bufferingNewest(1))
        let id = UUID()
        subscribers[id] = continuation
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor in self?.subscribers[id] = nil }
        }
        return stream
    }

    func beginNavigation() {
        manager.desiredAccuracy = kCLLocationAccuracyBest
        manager.activityType = .otherNavigation
        manager.distanceFilter = 3
        manager.pausesLocationUpdatesAutomatically = false
        manager.allowsBackgroundLocationUpdates = true
        manager.showsBackgroundLocationIndicator = true
        backgroundSession = backgroundSession ?? CLBackgroundActivitySession()
        requestAuthorizationIfNeeded()
        if isAuthorized { manager.startUpdatingLocation() }
    }

    func endNavigation() {
        backgroundSession?.invalidate()
        backgroundSession = nil
        manager.allowsBackgroundLocationUpdates = false
        manager.showsBackgroundLocationIndicator = false
        manager.pausesLocationUpdatesAutomatically = true
        manager.desiredAccuracy = kCLLocationAccuracyNearestTenMeters
        manager.activityType = .other
        manager.distanceFilter = kCLDistanceFilterNone
    }

    private func resumeWaiters(with result: Result<Coordinate, Error>) {
        let pending = waiters
        waiters = []
        pending.forEach { $0.resume(with: result) }
    }
}

extension LocationService: CLLocationManagerDelegate {
    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        let status = manager.authorizationStatus
        MainActor.assumeIsolated {
            authorizationStatus = status
            switch status {
            case .authorizedWhenInUse, .authorizedAlways:
                startUpdates()
                if !waiters.isEmpty { self.manager.requestLocation() }
            case .denied:
                resumeWaiters(with: .failure(LocationError.denied))
            case .restricted:
                resumeWaiters(with: .failure(LocationError.restricted))
            default:
                break
            }
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let location = locations.last else { return }
        let coordinate = Coordinate(location.coordinate)
        let fix = LocationFix(
            coordinate: coordinate,
            horizontalAccuracy: location.horizontalAccuracy,
            timestamp: location.timestamp
        )
        MainActor.assumeIsolated {
            self.coordinate = coordinate
            resumeWaiters(with: .success(coordinate))
            subscribers.values.forEach { $0.yield(fix) }
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateHeading newHeading: CLHeading) {
        guard newHeading.headingAccuracy >= 0 else { return }
        let value = newHeading.trueHeading >= 0 ? newHeading.trueHeading : newHeading.magneticHeading
        MainActor.assumeIsolated {
            heading = value
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        let code = (error as? CLError)?.code
        guard code != .locationUnknown else { return }
        let denied = code == .denied
        MainActor.assumeIsolated {
            if let coordinate, !denied {
                resumeWaiters(with: .success(coordinate))
            } else if !waiters.isEmpty {
                resumeWaiters(with: .failure(denied ? LocationError.denied : LocationError.unavailable))
            }
        }
    }
}
